// Boot-disk health and temperature from the NVMe SMART / Health log
// (log page 0x02), read through Apple's NVMeSMARTLib IOKit plug-in — the
// same read-only, unprivileged path smartmontools uses on macOS. Nothing here
// writes to the drive, runs self-tests or changes settings. When the drive is
// not NVMe or the plug-in refuses, health falls back to the "SMART Status"
// that `diskutil info -plist /` reports, with every counter left None.

use super::disk::{boot_bsd_name, boot_file_system};
use super::ffi::{run_bounded, IoObject};
use super::DiskHealth;
use core_foundation::base::TCFType;
use core_foundation::uuid::{CFUUIDBytes, CFUUIDGetUUIDBytes, CFUUIDRef, CFUUID};
use std::collections::HashSet;
use std::ffi::c_void;
use std::time::Duration;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOCreatePlugInInterfaceForService(
        service: u32,
        plugin_type: CFUUIDRef,
        interface_type: CFUUIDRef,
        plugin: *mut *mut *mut IUnknown,
        score: *mut i32,
    ) -> i32;
    fn IODestroyPlugInInterface(plugin: *mut *mut IUnknown) -> i32;
}

/// COM-style IUnknown vtable head (IUNKNOWN_C_GUTS).
#[repr(C)]
struct IUnknown {
    reserved: *mut c_void,
    query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
}

/// Head of IONVMeSMARTInterface (NVMeSMARTLibExternal.h); only the first
/// method is used: SMARTReadData(interface, NVMeSMARTData*).
#[repr(C)]
struct NvmeSmartInterface {
    unknown: IUnknown,
    version: u16,
    revision: u16,
    smart_read_data: Option<unsafe extern "C" fn(*mut c_void, *mut u8) -> i32>,
}

/// kIOCFPlugInInterfaceID
const PLUGIN_INTERFACE: [u8; 16] = [
    0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F,
];
/// kIONVMeSMARTUserClientTypeID
const NVME_SMART_CLIENT: [u8; 16] = [
    0xAA, 0x0F, 0xA6, 0xF9, 0xC2, 0xD6, 0x45, 0x7F, 0xB1, 0x0B, 0x59, 0xA1, 0x32, 0x53, 0x29, 0x2F,
];
/// kIONVMeSMARTInterfaceID
const NVME_SMART_INTERFACE: [u8; 16] = [
    0xCC, 0xD1, 0xDB, 0x19, 0xFD, 0x9A, 0x4D, 0xAF, 0xBF, 0x95, 0x12, 0x45, 0x4B, 0x23, 0x0A, 0xB6,
];

/// CFUUID from its 16 bytes.
fn uuid(bytes: [u8; 16]) -> CFUUID {
    // CFUUIDBytes is exactly sixteen consecutive u8 fields.
    let raw: CFUUIDBytes = unsafe { std::mem::transmute(bytes) };
    unsafe {
        CFUUID::wrap_under_create_rule(core_foundation::uuid::CFUUIDCreateFromUUIDBytes(
            std::ptr::null(),
            raw,
        ))
    }
}

/// The single physical IOBlockStorageDevice under the boot volume. Walks the
/// IOService parents from the volume's IOMedia (APFS volume → container →
/// physical store → disk → driver → device). Fusion/RAID sets have several
/// devices and get None: there is no single disk to report on.
fn boot_device() -> Option<IoObject> {
    let media = IoObject::bsd_media(&boot_bsd_name()?)?;
    let mut pending = vec![media];
    let mut seen = HashSet::new();
    let mut devices = Vec::new();
    while let Some(entry) = pending.pop() {
        let id = entry.registry_id()?;
        if !seen.insert(id) || seen.len() > 128 {
            continue;
        }
        if entry.conforms_to(c"IOBlockStorageDevice") {
            devices.push(entry);
            continue;
        }
        pending.extend(entry.parents());
    }
    (devices.len() == 1).then(|| devices.pop())?
}

/// Read the raw 512-byte NVMe SMART / Health log of the boot disk.
fn read_nvme_log() -> Option<[u8; 512]> {
    let device = boot_device()?;
    if device.boolean("NVMe SMART Capable") != Some(true) {
        return None;
    }
    let client = uuid(NVME_SMART_CLIENT);
    let plugin_iface = uuid(PLUGIN_INTERFACE);
    let iface_id = uuid(NVME_SMART_INTERFACE);
    let mut plugin: *mut *mut IUnknown = std::ptr::null_mut();
    let mut score = 0;
    let rc = unsafe {
        IOCreatePlugInInterfaceForService(
            device.0,
            client.as_concrete_TypeRef(),
            plugin_iface.as_concrete_TypeRef(),
            &mut plugin,
            &mut score,
        )
    };
    if rc != 0 || plugin.is_null() {
        return None;
    }
    // Owned plug-in; destroyed on every exit path.
    struct Plugin(*mut *mut IUnknown);
    impl Drop for Plugin {
        /// Tear down the plug-in instance.
        fn drop(&mut self) {
            unsafe { IODestroyPlugInInterface(self.0) };
        }
    }
    let plugin = Plugin(plugin);
    let mut iface: *mut c_void = std::ptr::null_mut();
    let rc = unsafe {
        ((**plugin.0).query_interface)(
            plugin.0.cast(),
            CFUUIDGetUUIDBytes(iface_id.as_concrete_TypeRef()),
            &mut iface,
        )
    };
    if rc != 0 || iface.is_null() {
        return None;
    }
    let iface = iface as *mut *mut NvmeSmartInterface;
    // Owned interface reference from QueryInterface; released on drop.
    struct Iface(*mut *mut NvmeSmartInterface);
    impl Drop for Iface {
        /// Balance QueryInterface's AddRef.
        fn drop(&mut self) {
            unsafe { ((**self.0).unknown.release)(self.0.cast()) };
        }
    }
    let iface = Iface(iface);
    let read = unsafe { (**iface.0).smart_read_data }?;
    let mut log = [0u8; 512];
    let rc = unsafe { read(iface.0.cast(), log.as_mut_ptr()) };
    (rc == 0).then_some(log)
}

/// Little-endian 128-bit counter field, saturated to u64.
fn le_u128(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&bytes[..16]);
    u64::try_from(u128::from_le_bytes(buf)).unwrap_or(u64::MAX)
}

/// Composite temperature (Kelvin, bytes 1–2) in °C, when plausible.
pub(crate) fn temperature_from_log(log: &[u8; 512]) -> Option<f32> {
    let kelvin = u16::from_le_bytes([log[1], log[2]]);
    let celsius = f32::from(kelvin) - 273.15;
    (kelvin != 0 && celsius > 0.0 && celsius <= 150.0).then_some(celsius)
}

/// Decode the NVMe Health Information log (NVMe base spec, log page 02h).
pub(crate) fn health_from_log(log: &[u8; 512]) -> DiskHealth {
    let critical = log[0];
    let spare = log[3];
    let used = log[5];
    // One "data unit" is 1000 × 512-byte sectors.
    let units = |off: usize| le_u128(&log[off..off + 16]).saturating_mul(512_000);
    // Bits: 0 spare below threshold, 2 reliability degraded, 3 read-only →
    // the drive is failing; 1 temperature, 4 volatile backup, 5 PMR → warn.
    let status = if critical & 0b0000_1101 != 0 {
        "failing"
    } else if critical != 0 || used >= 100 {
        "warning"
    } else {
        "verified"
    };
    DiskHealth {
        status: status.into(),
        file_system: None,
        health_percent: Some(100u8.saturating_sub(used.min(100))),
        temp_c: temperature_from_log(log),
        data_read_bytes: Some(units(32)),
        data_written_bytes: Some(units(48)),
        power_on_hours: Some(le_u128(&log[128..144])),
        power_cycles: Some(le_u128(&log[112..128])),
        available_spare_percent: (spare <= 100).then_some(spare),
        media_errors: Some(le_u128(&log[160..176])),
        critical_warning: Some(critical),
    }
}

/// Boot-disk NVMe composite temperature, °C.
pub(crate) fn temperature() -> Option<f32> {
    temperature_from_log(&read_nvme_log()?)
}

/// Map diskutil's "SMARTStatus" string to our status vocabulary.
pub(crate) fn status_from_diskutil(value: &str) -> &'static str {
    match value {
        "Verified" => "verified",
        "Failing" => "failing",
        "About to Fail" => "warning",
        _ => "unknown",
    }
}

/// diskutil's SMART verdict for the boot volume (bounded subprocess).
fn diskutil_status() -> Option<String> {
    let out = run_bounded(
        "/usr/sbin/diskutil",
        &["info", "-plist", "/"],
        Duration::from_secs(5),
    )?;
    let value: plist::Value = plist::from_bytes(&out).ok()?;
    let status = value.as_dictionary()?.get("SMARTStatus")?.as_string()?;
    Some(status_from_diskutil(status).to_string())
}

/// Full health of the boot disk (expensive-ish: call at most every 5 min).
pub(crate) fn read() -> DiskHealth {
    let mut health = match read_nvme_log() {
        Some(log) => health_from_log(&log),
        None => DiskHealth {
            status: diskutil_status().unwrap_or_else(|| "unknown".into()),
            ..DiskHealth::default()
        },
    };
    health.file_system = boot_file_system();
    health
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic healthy log: 35 °C, 100 % spare, 3 % used.
    fn sample_log() -> [u8; 512] {
        let mut log = [0u8; 512];
        log[1..3].copy_from_slice(&(308u16).to_le_bytes());
        log[3] = 100;
        log[4] = 10;
        log[5] = 3;
        log[32..48].copy_from_slice(&(2_000u128).to_le_bytes());
        log[48..64].copy_from_slice(&(1_000u128).to_le_bytes());
        log[112..128].copy_from_slice(&(42u128).to_le_bytes());
        log[128..144].copy_from_slice(&(1234u128).to_le_bytes());
        log
    }

    #[test]
    fn decodes_health_log() {
        let h = health_from_log(&sample_log());
        assert_eq!(h.status, "verified");
        assert_eq!(h.health_percent, Some(97));
        assert!((h.temp_c.unwrap() - 34.85).abs() < 0.01);
        assert_eq!(h.data_read_bytes, Some(2_000 * 512_000));
        assert_eq!(h.data_written_bytes, Some(1_000 * 512_000));
        assert_eq!(h.power_cycles, Some(42));
        assert_eq!(h.power_on_hours, Some(1234));
        assert_eq!(h.available_spare_percent, Some(100));
        assert_eq!(h.media_errors, Some(0));
        assert_eq!(h.critical_warning, Some(0));
    }

    #[test]
    fn critical_warning_bits_drive_status() {
        let mut log = sample_log();
        log[0] = 0b10; // temperature
        assert_eq!(health_from_log(&log).status, "warning");
        log[0] = 0b100; // reliability degraded
        assert_eq!(health_from_log(&log).status, "failing");
        log[0] = 0;
        log[5] = 120; // endurance exhausted
        let h = health_from_log(&log);
        assert_eq!(h.status, "warning");
        assert_eq!(h.health_percent, Some(0));
    }

    #[test]
    fn temperature_rejects_missing_and_implausible() {
        let mut log = [0u8; 512];
        assert_eq!(temperature_from_log(&log), None);
        log[1..3].copy_from_slice(&(200u16).to_le_bytes());
        assert_eq!(temperature_from_log(&log), None);
        log[1..3].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(temperature_from_log(&log), None);
    }

    #[test]
    fn huge_counters_saturate() {
        let mut log = sample_log();
        log[32..48].copy_from_slice(&u128::MAX.to_le_bytes());
        assert_eq!(health_from_log(&log).data_read_bytes, Some(u64::MAX));
    }

    #[test]
    fn diskutil_status_mapping() {
        assert_eq!(status_from_diskutil("Verified"), "verified");
        assert_eq!(status_from_diskutil("Failing"), "failing");
        assert_eq!(status_from_diskutil("Not Supported"), "unknown");
    }

    /// Smoke: never panics; whatever the drive reports stays plausible.
    #[test]
    fn live_read_fails_closed() {
        let h = read();
        assert!(["verified", "warning", "failing", "unknown"].contains(&h.status.as_str()));
        if let Some(t) = h.temp_c {
            assert!(t > 0.0 && t <= 150.0);
        }
    }
}
