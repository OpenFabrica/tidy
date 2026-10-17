// Boot-volume capacity and system-wide block I/O.
//
// Capacity uses the same CFURL volume keys Finder uses, so "available"
// includes purgeable space (NSURLVolumeAvailableCapacityForImportantUsageKey).
// I/O comes from every IOBlockStorageDriver's "Statistics" dictionary —
// driver level, so APFS volumes sharing a disk are not double counted; disk
// images are skipped because their I/O already lands on a physical disk.

use super::ffi::{dict_int, IoObject};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::{CFURLRef, CFURL};
use std::collections::HashMap;

extern "C" {
    static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFStringRef;
    static kCFURLVolumeAvailableCapacityKey: CFStringRef;
    static kCFURLVolumeTotalCapacityKey: CFStringRef;
    static kCFURLVolumeLocalizedNameKey: CFStringRef;
    fn CFURLCopyResourcePropertyForKey(
        url: CFURLRef,
        key: CFStringRef,
        value: *mut CFTypeRef,
        error: *mut *mut std::ffi::c_void,
    ) -> u8;
}

/// Boot volume capacity, bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Capacity {
    pub name: String,
    pub total: u64,
    pub available: u64,
}

impl Capacity {
    /// Used share of the volume, 0–100.
    pub(crate) fn used_percent(&self) -> Option<f32> {
        (self.total > 0).then(|| {
            let used = self.total.saturating_sub(self.available);
            (used as f64 / self.total as f64 * 100.0) as f32
        })
    }
}

/// One CFURL resource value; a fresh URL each call so CF's per-URL value
/// cache never serves an old number.
fn resource(url: &CFURL, key: CFStringRef) -> Option<CFType> {
    let mut value: CFTypeRef = std::ptr::null();
    let ok = unsafe {
        CFURLCopyResourcePropertyForKey(
            url.as_concrete_TypeRef(),
            key,
            &mut value,
            std::ptr::null_mut(),
        )
    };
    (ok != 0 && !value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

/// Integer resource value.
fn resource_u64(url: &CFURL, key: CFStringRef) -> Option<u64> {
    resource(url, key)?
        .downcast::<CFNumber>()?
        .to_i64()
        .and_then(|v| u64::try_from(v).ok())
}

/// Capacity of the volume mounted at `/` (Finder's numbers). Falls back to
/// statfs free space when the important-usage estimate is unavailable.
pub(crate) fn capacity() -> Option<Capacity> {
    let url = CFURL::from_path("/", true)?;
    let total = unsafe { resource_u64(&url, kCFURLVolumeTotalCapacityKey) }?;
    let available = unsafe {
        resource_u64(&url, kCFURLVolumeAvailableCapacityForImportantUsageKey)
            .filter(|v| *v > 0)
            .or_else(|| resource_u64(&url, kCFURLVolumeAvailableCapacityKey))
    }?;
    let name = unsafe { resource(&url, kCFURLVolumeLocalizedNameKey) }
        .and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string())
        .unwrap_or_default();
    Some(Capacity {
        name,
        total,
        available: available.min(total),
    })
}

/// Cumulative read/write bytes summed over physical block devices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct IoCounters {
    /// Per driver registry id: (read, written).
    pub devices: HashMap<u64, (u64, u64)>,
}

impl IoCounters {
    /// System-wide totals since boot.
    pub(crate) fn totals(&self) -> (u64, u64) {
        self.devices.values().fold((0, 0), |(r, w), (dr, dw)| {
            (r.saturating_add(*dr), w.saturating_add(*dw))
        })
    }
}

/// Bytes moved between two counter snapshots, per device. A device that is
/// new or whose counters went backwards (re-attached) contributes nothing
/// instead of a bogus spike.
pub(crate) fn io_delta(prev: &IoCounters, cur: &IoCounters) -> (u64, u64) {
    cur.devices
        .iter()
        .filter_map(|(id, (r, w))| {
            let (pr, pw) = prev.devices.get(id)?;
            Some((r.checked_sub(*pr)?, w.checked_sub(*pw)?))
        })
        .fold((0, 0), |(a, b), (r, w)| {
            (a.saturating_add(r), b.saturating_add(w))
        })
}

/// Reads I/O counters; remembers which drivers sit on disk images.
#[derive(Default)]
pub(crate) struct IoReader {
    is_image: HashMap<u64, bool>,
}

impl IoReader {
    /// Snapshot every physical driver's counters; None when IOKit lists none.
    pub(crate) fn read(&mut self) -> Option<IoCounters> {
        let mut devices = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        for driver in IoObject::services(c"IOBlockStorageDriver") {
            let Some(id) = driver.registry_id() else {
                continue;
            };
            seen.insert(id);
            let image = *self.is_image.entry(id).or_insert_with(|| {
                driver.parent().is_some_and(|device| {
                    device
                        .dict("Device Characteristics")
                        .and_then(|d| {
                            d.find(CFString::new("Product Name"))
                                .and_then(|v| v.downcast::<CFString>())
                                .map(|s| s.to_string())
                        })
                        .is_some_and(|name| name == "Disk Image")
                })
            });
            if image {
                continue;
            }
            let Some(stats) = driver.dict("Statistics") else {
                continue;
            };
            let read = dict_int(&stats, "Bytes (Read)").and_then(|v| u64::try_from(v).ok());
            let write = dict_int(&stats, "Bytes (Write)").and_then(|v| u64::try_from(v).ok());
            if let (Some(r), Some(w)) = (read, write) {
                devices.insert(id, (r, w));
            }
        }
        // Forget drivers that went away so the cache cannot grow unbounded.
        self.is_image.retain(|id, _| seen.contains(id));
        (!devices.is_empty()).then_some(IoCounters { devices })
    }
}

/// BSD device of the boot volume ("disk3s1s1") via statfs("/").
pub(crate) fn boot_bsd_name() -> Option<String> {
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c"/".as_ptr(), &mut st) } != 0 {
        return None;
    }
    let from = unsafe { std::ffi::CStr::from_ptr(st.f_mntfromname.as_ptr()) }.to_string_lossy();
    from.strip_prefix("/dev/").map(str::to_string)
}

/// File system of the boot volume as users know it ("APFS", "HFS+").
pub(crate) fn boot_file_system() -> Option<String> {
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c"/".as_ptr(), &mut st) } != 0 {
        return None;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy();
    Some(match raw.as_ref() {
        "apfs" => "APFS".to_string(),
        "hfs" => "HFS+".to_string(),
        "exfat" => "ExFAT".to_string(),
        "msdos" => "FAT".to_string(),
        other => other.to_uppercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_delta_ignores_new_and_reset_devices() {
        let prev = IoCounters {
            devices: HashMap::from([(1, (100, 200)), (2, (50, 50))]),
        };
        let cur = IoCounters {
            devices: HashMap::from([(1, (160, 260)), (2, (10, 10)), (3, (999, 999))]),
        };
        assert_eq!(io_delta(&prev, &cur), (60, 60));
        assert_eq!(cur.totals(), (1169, 1269));
    }

    #[test]
    fn used_percent_math() {
        let c = Capacity {
            name: "X".into(),
            total: 1000,
            available: 250,
        };
        assert_eq!(c.used_percent(), Some(75.0));
        assert_eq!(Capacity::default().used_percent(), None);
    }

    #[test]
    fn live_boot_volume_reads() {
        let c = capacity().expect("capacity of /");
        assert!(c.total > 0 && c.available <= c.total);
        assert!(boot_bsd_name().is_some_and(|n| n.starts_with("disk")));
        assert!(boot_file_system().is_some());
        let mut r = IoReader::default();
        let io = r.read().expect("block storage statistics");
        assert!(io.totals().0 > 0);
    }
}
