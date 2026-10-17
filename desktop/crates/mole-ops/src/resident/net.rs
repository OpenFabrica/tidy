// Network interface counters, default-route selection, interface details and
// persisted cumulative traffic.
//
// Counters come from the kernel's 64-bit interface table (sysctl
// NET_RT_IFLIST2 → if_data64), so they never wrap like getifaddrs' 32-bit
// if_data. The automatic interface is the first physical (en*) interface
// in network-service order (SystemConfiguration's primary interface, then
// the user's ServiceOrder — what `scutil --nwi` shows), falling back to the
// routing table. A VPN/TUN primary (utun) is skipped when a physical one is
// active, because Wi-Fi/Ethernet traffic and details are what users expect.
// MAC addresses come from IOKit (getifaddrs masks them on recent macOS);
// Wi-Fi PHY mode and channel from CoreWLAN, which needs no Location access.

use super::ffi::{sysctl_mib, IoObject};
use super::objc;
use super::NetDetails;
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Cumulative (rx, tx) bytes per interface name.
pub(crate) type Counters = HashMap<String, (u64, u64)>;

/// Round a sockaddr length up to the 4-byte boundary routing messages use.
fn sa_round(len: usize) -> usize {
    if len == 0 {
        4
    } else {
        (len + 3) & !3
    }
}

/// Interface byte counters for every interface (loopback excluded).
pub(crate) fn read_counters() -> Option<Counters> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let buf = sysctl_mib(&mut mib)?;
    Some(parse_iflist2(&buf))
}

/// Parse RTM_IFINFO2 messages: header (if_msghdr2) followed by the link-level
/// sockaddr_dl carrying the interface name. Other message types are skipped
/// by their own length.
pub(crate) fn parse_iflist2(buf: &[u8]) -> Counters {
    let mut out = Counters::new();
    let mut off = 0;
    let hdr_len = std::mem::size_of::<libc::if_msghdr2>();
    while off + 4 <= buf.len() {
        let msglen = usize::from(u16::from_ne_bytes([buf[off], buf[off + 1]]));
        if msglen < 4 || off + msglen > buf.len() {
            break;
        }
        let msg = &buf[off..off + msglen];
        if i32::from(msg[3]) == libc::RTM_IFINFO2 && msglen >= hdr_len {
            // if_msghdr2 is packed(4); read it unaligned from the byte buffer.
            let hdr: libc::if_msghdr2 =
                unsafe { std::ptr::read_unaligned(msg.as_ptr().cast::<libc::if_msghdr2>()) };
            let name = (hdr.ifm_addrs & libc::RTA_IFP != 0)
                .then(|| link_name(&msg[hdr_len..]))
                .flatten();
            if let Some(name) = name.filter(|n| !n.starts_with("lo")) {
                let data = hdr.ifm_data;
                out.insert(name, (data.ifi_ibytes, data.ifi_obytes));
            }
        }
        off += msglen;
    }
    out
}

/// Interface name from a sockaddr_dl (sdl_nlen bytes of sdl_data).
fn link_name(sa: &[u8]) -> Option<String> {
    // sockaddr_dl: len, family, index(2), type, nlen, alen, slen, data...
    if sa.len() < 8 || i32::from(sa[1]) != libc::AF_LINK {
        return None;
    }
    let nlen = usize::from(sa[5]);
    let name = sa.get(8..8 + nlen)?;
    let name = String::from_utf8_lossy(name).into_owned();
    (!name.is_empty()).then_some(name)
}

/// IPv4 default-route interfaces in routing-table order:
/// (interface index, interface-scoped?).
fn default_routes() -> Vec<(u16, bool)> {
    let mut mib = [
        libc::CTL_NET,
        libc::PF_ROUTE,
        0,
        libc::AF_INET,
        libc::NET_RT_DUMP,
        0,
    ];
    let Some(buf) = sysctl_mib(&mut mib) else {
        return Vec::new();
    };
    parse_default_routes(&buf)
}

/// Pick default routes out of an RTM dump: destination 0.0.0.0 with an
/// empty or all-zero netmask (split /1 VPN routes are not defaults).
pub(crate) fn parse_default_routes(buf: &[u8]) -> Vec<(u16, bool)> {
    let hdr_len = std::mem::size_of::<libc::rt_msghdr>();
    let mut out = Vec::new();
    let mut off = 0;
    while off + 4 <= buf.len() {
        let msglen = usize::from(u16::from_ne_bytes([buf[off], buf[off + 1]]));
        if msglen < 4 || off + msglen > buf.len() {
            break;
        }
        let msg = &buf[off..off + msglen];
        off += msglen;
        if msglen < hdr_len {
            continue;
        }
        let hdr: libc::rt_msghdr =
            unsafe { std::ptr::read_unaligned(msg.as_ptr().cast::<libc::rt_msghdr>()) };
        if hdr.rtm_flags & libc::RTF_UP == 0 {
            continue;
        }
        // Walk the sockaddrs present in rtm_addrs (RTA_DST, RTA_GATEWAY,
        // RTA_NETMASK, ...), each padded to 4 bytes.
        let mut pos = hdr_len;
        let mut dst_default = false;
        let mut mask_zero = true;
        for bit in 0..libc::RTAX_MAX {
            if hdr.rtm_addrs & (1 << bit) == 0 {
                continue;
            }
            let Some(&sa_len) = msg.get(pos) else { break };
            let sa_len = usize::from(sa_len);
            let sa = msg.get(pos..pos + sa_len.max(2)).unwrap_or(&[]);
            match 1 << bit {
                libc::RTA_DST => {
                    // sockaddr_in: len, family, port(2), addr(4)
                    dst_default = sa.len() >= 8
                        && i32::from(sa[1]) == libc::AF_INET
                        && sa[4..8] == [0, 0, 0, 0];
                }
                libc::RTA_NETMASK => {
                    mask_zero = sa_len <= 4 || sa.iter().skip(4).all(|b| *b == 0);
                }
                _ => {}
            }
            pos += sa_round(sa_len);
        }
        if dst_default && mask_zero {
            out.push((hdr.rtm_index, hdr.rtm_flags & libc::RTF_IFSCOPE != 0));
        }
    }
    out
}

/// Interface name for an index.
fn index_name(index: u16) -> Option<String> {
    let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
    let ptr = unsafe { libc::if_indextoname(u32::from(index), buf.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Physical interfaces are the en* family (Wi-Fi, Ethernet, Thunderbolt,
/// USB tethering); utun/ipsec/ppp/bridge/awdl/llw are virtual.
pub(crate) fn is_physical(name: &str) -> bool {
    name.starts_with("en")
}

/// Choose the automatic interface from default-route names in table order:
/// first physical one, else the first default at all.
pub(crate) fn choose_default(names: &[String]) -> Option<String> {
    names
        .iter()
        .find(|n| is_physical(n))
        .or_else(|| names.first())
        .cloned()
}

/// SCDynamicStoreCopyValue's signature.
type CopyValue = unsafe extern "C" fn(*const c_void, CFStringRef) -> CFTypeRef;

/// SystemConfiguration entry points, resolved at runtime with dlsym so the
/// framework stays out of mole-helper's load commands.
struct DynamicStore {
    copy_value: CopyValue,
    /// SCDynamicStoreRef, kept for the process lifetime.
    store: usize,
}

/// The process-wide dynamic-store session; None when SC cannot load.
fn dynamic_store() -> Option<&'static DynamicStore> {
    static STORE: OnceLock<Option<DynamicStore>> = OnceLock::new();
    STORE
        .get_or_init(|| unsafe {
            let path =
                c"/System/Library/Frameworks/SystemConfiguration.framework/SystemConfiguration";
            let lib = libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
            if lib.is_null() {
                return None;
            }
            let create = libc::dlsym(lib, c"SCDynamicStoreCreate".as_ptr());
            let copy = libc::dlsym(lib, c"SCDynamicStoreCopyValue".as_ptr());
            if create.is_null() || copy.is_null() {
                return None;
            }
            type Create = unsafe extern "C" fn(
                *const c_void,
                CFStringRef,
                *const c_void,
                *const c_void,
            ) -> *const c_void;
            let create: Create = std::mem::transmute(create);
            let name = CFString::new("Tidy resident");
            let store = create(
                std::ptr::null(),
                name.as_concrete_TypeRef(),
                std::ptr::null(),
                std::ptr::null(),
            );
            (!store.is_null()).then(|| DynamicStore {
                copy_value: std::mem::transmute::<*mut c_void, CopyValue>(copy),
                store: store as usize,
            })
        })
        .as_ref()
}

/// One dynamic-store dictionary, e.g. "State:/Network/Global/IPv4".
fn store_dict(key: &str) -> Option<CFDictionary<CFString, CFType>> {
    let ds = dynamic_store()?;
    let key = CFString::new(key);
    let raw = unsafe { (ds.copy_value)(ds.store as *const c_void, key.as_concrete_TypeRef()) };
    if raw.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(raw) };
    let dict = value.downcast::<CFDictionary>()?;
    Some(unsafe { CFDictionary::wrap_under_get_rule(dict.as_concrete_TypeRef()) })
}

/// String value in a dynamic-store dictionary.
fn dict_string(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<String> {
    Some(
        dict.find(CFString::new(key))?
            .downcast::<CFString>()?
            .to_string(),
    )
}

/// Network service ids in the user's service order (System Settings order).
fn service_order() -> Vec<String> {
    store_dict("Setup:/Network/Global/IPv4")
        .and_then(|setup| {
            let arr = setup
                .find(CFString::new("ServiceOrder"))?
                .downcast::<CFArray>()?;
            Some(
                arr.iter()
                    .filter_map(|v| {
                        let v = unsafe { CFType::wrap_under_get_rule(*v) };
                        v.downcast::<CFString>().map(|s| s.to_string())
                    })
                    .take(64)
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default()
}

/// Active IPv4 interfaces in priority order: the primary interface, then
/// every active service in the user's service order.
fn service_order_interfaces() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(global) = store_dict("State:/Network/Global/IPv4") {
        out.extend(dict_string(&global, "PrimaryInterface"));
    }
    for id in service_order() {
        let Some(state) = store_dict(&format!("State:/Network/Service/{id}/IPv4")) else {
            continue;
        };
        if let Some(name) = dict_string(&state, "InterfaceName") {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// One choice for the settings "network interface" selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    /// BSD name, the value stored in prefs ("en0").
    pub name: String,
    /// Network service name ("Wi-Fi", "Thunderbolt Ethernet"), else `name`.
    pub display: String,
    /// The service currently has an IPv4 configuration (link up, addressed).
    pub active: bool,
}

/// A network service as SystemConfiguration describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceEntry {
    pub name: String,
    pub display: String,
    pub active: bool,
    /// HiddenConfiguration services (internal adapters) are not user-facing.
    pub hidden: bool,
}

/// Keep user-facing interfaces in service order: physical (en*) services
/// that are not hidden-and-idle, plus the primary interface whatever its
/// kind (a VPN/TUN primary carries the user's traffic). The primary comes
/// first when it is not itself an ordered service; duplicates are dropped.
pub(crate) fn select_interfaces(
    services: Vec<ServiceEntry>,
    primary: Option<ServiceEntry>,
) -> Vec<InterfaceInfo> {
    let primary_name = primary.as_ref().map(|p| p.name.clone());
    let mut out: Vec<InterfaceInfo> = Vec::new();
    let push = |e: ServiceEntry, out: &mut Vec<InterfaceInfo>| {
        if !out.iter().any(|i| i.name == e.name) {
            out.push(InterfaceInfo {
                display: if e.display.trim().is_empty() {
                    e.name.clone()
                } else {
                    e.display
                },
                name: e.name,
                active: e.active,
            });
        }
    };
    if let Some(p) = primary {
        if !services.iter().any(|s| s.name == p.name) {
            push(p, &mut out);
        }
    }
    for s in services {
        let is_primary = primary_name.as_deref() == Some(s.name.as_str());
        if is_primary || (is_physical(&s.name) && (!s.hidden || s.active)) {
            push(s, &mut out);
        }
    }
    out
}

/// User-facing network interfaces for the settings selector, in service
/// order (see `select_interfaces`). Empty when SystemConfiguration is
/// unavailable.
pub fn list_interfaces() -> Vec<InterfaceInfo> {
    let display_of = |id: &str, iface: Option<&CFDictionary<CFString, CFType>>| {
        store_dict(&format!("Setup:/Network/Service/{id}"))
            .and_then(|d| dict_string(&d, "UserDefinedName"))
            .or_else(|| iface.and_then(|d| dict_string(d, "UserDefinedName")))
            .unwrap_or_default()
    };
    let services: Vec<ServiceEntry> = service_order()
        .into_iter()
        .filter_map(|id| {
            let iface = store_dict(&format!("Setup:/Network/Service/{id}/Interface"))?;
            let name = dict_string(&iface, "DeviceName")?;
            let hidden = iface
                .find(CFString::new("HiddenConfiguration"))
                .and_then(|v| v.downcast::<core_foundation::boolean::CFBoolean>())
                .is_some_and(bool::from);
            let active = store_dict(&format!("State:/Network/Service/{id}/IPv4"))
                .and_then(|st| dict_string(&st, "InterfaceName"))
                .is_some();
            Some(ServiceEntry {
                display: display_of(&id, Some(&iface)),
                name,
                active,
                hidden,
            })
        })
        .collect();
    let primary = store_dict("State:/Network/Global/IPv4").and_then(|g| {
        let name = dict_string(&g, "PrimaryInterface")?;
        let display = dict_string(&g, "PrimaryService")
            .map(|id| display_of(&id, None))
            .unwrap_or_default();
        Some(ServiceEntry {
            name,
            display,
            active: true,
            hidden: false,
        })
    });
    select_interfaces(services, primary)
}

/// Automatic interface: service order first, routing table as fallback;
/// physical preferred in both.
pub(crate) fn default_interface() -> Option<String> {
    let ordered = service_order_interfaces();
    if let Some(name) = choose_default(&ordered) {
        return Some(name);
    }
    let names: Vec<String> = default_routes()
        .into_iter()
        .filter_map(|(idx, _)| index_name(idx))
        .collect();
    choose_default(&names)
}

/// Addresses and link state for one interface (getifaddrs).
fn addresses(name: &str) -> (bool, Vec<String>, Vec<String>) {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return (false, Vec::new(), Vec::new());
    }
    let mut up = false;
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    let mut cur = list;
    let mut guard = 0;
    while !cur.is_null() && guard < 4096 {
        guard += 1;
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_name.is_null() || ifa.ifa_addr.is_null() {
            continue;
        }
        if unsafe { CStr::from_ptr(ifa.ifa_name) }.to_bytes() != name.as_bytes() {
            continue;
        }
        let flags = ifa.ifa_flags as i32;
        up |= flags & libc::IFF_UP != 0 && flags & libc::IFF_RUNNING != 0;
        match i32::from(unsafe { (*ifa.ifa_addr).sa_family }) {
            libc::AF_INET => {
                let sin = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in>() };
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_unspecified() && !ip.is_loopback() {
                    v4.push(ip.to_string());
                }
            }
            libc::AF_INET6 => {
                let sin6 = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in6>() };
                let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                // Link-local fe80:: addresses are noise for users.
                if !ip.is_unspecified() && !ip.is_loopback() && !ip.is_unicast_link_local() {
                    v6.push(ip.to_string());
                }
            }
            _ => {}
        }
    }
    unsafe { libc::freeifaddrs(list) };
    v4.dedup();
    v6.dedup();
    (up, v4, v6)
}

/// Hardware MAC from the IOKit controller behind the BSD interface.
fn mac_address(name: &str) -> Option<String> {
    IoObject::services(c"IONetworkInterface")
        .into_iter()
        .find(|iface| iface.string("BSD Name").as_deref() == Some(name))
        .and_then(|iface| iface.parent())
        .and_then(|controller| controller.data("IOMACAddress"))
        .filter(|b| b.len() == 6 && b.iter().any(|x| *x != 0))
        .map(|b| {
            b.iter()
                .map(|x| format!("{x:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        })
}

/// Human Wi-Fi standard for a CWPHYMode value.
pub(crate) fn phy_mode_label(mode: isize) -> Option<&'static str> {
    Some(match mode {
        1 => "802.11a",
        2 => "802.11b",
        3 => "802.11g",
        4 => "Wi-Fi 4 (802.11n)",
        5 => "Wi-Fi 5 (802.11ac)",
        6 => "Wi-Fi 6 (802.11ax)",
        7 => "Wi-Fi 7 (802.11be)",
        _ => return None,
    })
}

/// "36 (5 GHz, 80 MHz)" from CWChannel number / band / width enums.
pub(crate) fn channel_label(number: isize, band: isize, width: isize) -> Option<String> {
    if number <= 0 {
        return None;
    }
    let band = match band {
        1 => Some("2.4 GHz"),
        2 => Some("5 GHz"),
        3 => Some("6 GHz"),
        _ => None,
    };
    let width = match width {
        1 => Some("20 MHz"),
        2 => Some("40 MHz"),
        3 => Some("80 MHz"),
        4 => Some("160 MHz"),
        5 => Some("320 MHz"),
        _ => None,
    };
    let extra: Vec<&str> = [band, width].into_iter().flatten().collect();
    Some(if extra.is_empty() {
        number.to_string()
    } else {
        format!("{number} ({})", extra.join(", "))
    })
}

/// Wi-Fi standard + channel via CoreWLAN; None for non-Wi-Fi interfaces.
fn wifi_details(name: &str) -> (Option<String>, Option<String>) {
    if !objc::corewlan() {
        return (None, None);
    }
    objc::with_pool(|| unsafe {
        let client = objc::send_id(objc::class(c"CWWiFiClient"), c"sharedWiFiClient");
        let iface = objc::send_id_ptr(
            client,
            c"interfaceWithName:",
            objc::nsstring_from(name).cast_const(),
        );
        if iface.is_null() {
            return (None, None);
        }
        let standard = objc::send_isize(iface, c"activePHYMode")
            .and_then(phy_mode_label)
            .map(str::to_string);
        let channel = objc::send_id(iface, c"wlanChannel");
        let channel = if channel.is_null() {
            None
        } else {
            channel_label(
                objc::send_isize(channel, c"channelNumber").unwrap_or(0),
                objc::send_isize(channel, c"channelBand").unwrap_or(0),
                objc::send_isize(channel, c"channelWidth").unwrap_or(0),
            )
        };
        (standard, channel)
    })
}

/// Full details for one interface (panel Network tab; cached by the caller).
pub(crate) fn details(name: &str) -> NetDetails {
    let (connected, ipv4, ipv6) = addresses(name);
    let (wifi_standard, wifi_channel) = if connected {
        wifi_details(name)
    } else {
        (None, None)
    };
    NetDetails {
        interface: name.to_string(),
        connected,
        mac: mac_address(name),
        ipv4,
        ipv6,
        wifi_standard,
        wifi_channel,
    }
}

/// Persisted per-interface cumulative traffic (~/.config/mole/network-traffic.json).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Totals {
    #[serde(default)]
    pub interfaces: HashMap<String, InterfaceTotal>,
}

/// One interface's cumulative bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InterfaceTotal {
    pub rx: u64,
    pub tx: u64,
}

impl Totals {
    /// Path of the persisted totals under `home`.
    pub(crate) fn file(home: &str) -> PathBuf {
        PathBuf::from(home).join(".config/mole/network-traffic.json")
    }

    /// Load; empty on a missing or malformed file.
    pub(crate) fn load(home: &str) -> Totals {
        std::fs::read_to_string(Self::file(home))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Persist atomically (temp file + rename).
    pub(crate) fn save(&self, home: &str) -> std::io::Result<()> {
        let path = Self::file(home);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    /// Add the bytes moved since `prev` for every interface present in both
    /// snapshots. Counters that went backwards (interface re-created, driver
    /// reset) add nothing for that tick.
    pub(crate) fn accumulate(&mut self, prev: &Counters, cur: &Counters) {
        for (name, (rx, tx)) in cur {
            let Some((prx, ptx)) = prev.get(name) else {
                continue;
            };
            let drx = rx.checked_sub(*prx).unwrap_or(0);
            let dtx = tx.checked_sub(*ptx).unwrap_or(0);
            if drx == 0 && dtx == 0 {
                continue;
            }
            let t = self.interfaces.entry(name.clone()).or_default();
            t.rx = t.rx.saturating_add(drx);
            t.tx = t.tx.saturating_add(dtx);
        }
    }

    /// Totals for one interface (zero when never seen).
    pub(crate) fn get(&self, name: &str) -> InterfaceTotal {
        self.interfaces.get(name).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_adds_deltas_and_ignores_resets() {
        let mut t = Totals::default();
        let a = Counters::from([("en0".into(), (100, 50)), ("en7".into(), (10, 10))]);
        let b = Counters::from([
            ("en0".into(), (300, 80)),
            ("en7".into(), (5, 5)),     // reset → ignored
            ("utun4".into(), (99, 99)), // new → no baseline
        ]);
        t.accumulate(&a, &b);
        assert_eq!(t.get("en0"), InterfaceTotal { rx: 200, tx: 30 });
        assert_eq!(t.get("en7"), InterfaceTotal::default());
        assert_eq!(t.get("utun4"), InterfaceTotal::default());
    }

    #[test]
    fn totals_roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        assert_eq!(Totals::load(home), Totals::default());
        let mut t = Totals::default();
        t.interfaces
            .insert("en0".into(), InterfaceTotal { rx: 5, tx: 7 });
        t.save(home).unwrap();
        assert_eq!(Totals::load(home), t);
    }

    #[test]
    fn default_choice_prefers_physical() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            choose_default(&names(&["utun4", "en0", "en7"])).as_deref(),
            Some("en0")
        );
        assert_eq!(choose_default(&names(&["utun4"])).as_deref(), Some("utun4"));
        assert_eq!(choose_default(&[]), None);
    }

    #[test]
    fn labels_for_wifi() {
        assert_eq!(phy_mode_label(6), Some("Wi-Fi 6 (802.11ax)"));
        assert_eq!(phy_mode_label(0), None);
        assert_eq!(
            channel_label(36, 2, 3).as_deref(),
            Some("36 (5 GHz, 80 MHz)")
        );
        assert_eq!(channel_label(6, 0, 0).as_deref(), Some("6"));
        assert_eq!(channel_label(0, 1, 1), None);
    }

    #[test]
    fn iflist2_parser_reads_names_and_skips_garbage() {
        // Truncated / zero-length messages must not loop or panic.
        assert!(parse_iflist2(&[0, 0, 5, 0]).is_empty());
        assert!(parse_iflist2(&[200, 0, 5, 0x12]).is_empty());
        let live = read_counters().expect("interface table");
        assert!(!live.contains_key("lo0"));
    }

    fn entry(name: &str, display: &str, active: bool, hidden: bool) -> ServiceEntry {
        ServiceEntry {
            name: name.into(),
            display: display.into(),
            active,
            hidden,
        }
    }

    #[test]
    fn interface_filter_keeps_user_facing_in_service_order() {
        let services = vec![
            entry("en7", "USB LAN", true, false),
            entry("bridge0", "Thunderbolt Bridge", false, false),
            entry("en3", "Ethernet Adapter (en3)", false, true), // hidden, idle
            entry("en0", "Wi-Fi", true, false),
            entry("en9", "iPhone USB", false, false),
            entry("en7", "dup", true, false),
        ];
        let primary = Some(entry("utun4", "", true, false));
        let got = select_interfaces(services.clone(), primary);
        let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["utun4", "en7", "en0", "en9"]);
        assert_eq!(got[0].display, "utun4", "empty display falls back to name");
        assert_eq!(got[2].display, "Wi-Fi");
        assert!(!got[3].active);
        // A bridge that is the primary service stays (and keeps its slot).
        let got = select_interfaces(services, Some(entry("bridge0", "TB", true, false)));
        let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en7", "bridge0", "en0", "en9"]);
        // A hidden adapter that is active is shown.
        let got = select_interfaces(vec![entry("en3", "X", true, true)], None);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn live_list_interfaces_has_no_virtual_noise() {
        let list = list_interfaces();
        for i in &list {
            assert!(!i.name.is_empty() && !i.display.is_empty());
            let virt = ["lo", "awdl", "llw", "anpi", "ap", "utun", "bridge"]
                .iter()
                .any(|p| i.name.starts_with(p));
            assert!(
                !virt || i.active,
                "virtual {} listed while inactive",
                i.name
            );
        }
    }

    #[test]
    fn live_default_interface_is_listed() {
        // Machines without any network (CI sandboxes) may have none.
        if let Some(name) = default_interface() {
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn route_parser_survives_garbage_and_live_table() {
        assert!(parse_default_routes(&[0, 0, 5, 1]).is_empty());
        // Live: whatever comes back must name real interfaces.
        for (idx, _) in default_routes() {
            assert!(index_name(idx).is_some());
        }
    }
}
