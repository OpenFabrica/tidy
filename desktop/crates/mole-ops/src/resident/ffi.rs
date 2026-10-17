// Shared native plumbing for the resident samplers: IOKit registry reads
// (CF properties of IOService entries), sysctl helpers, and a bounded
// subprocess runner for the few system tools we still shell out to. Every
// helper fails closed (None/empty) so a missing sensor never becomes a number.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // Matching dictionaries are typed as *mut c_void to match smc.rs's
    // declarations of the same symbols (rustc flags clashing externs).
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOBSDNameMatching(port: u32, options: u32, name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, iterator: *mut u32) -> i32;
    fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> u32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IORegistryEntryGetParentIterator(
        entry: u32,
        plane: *const c_char,
        iterator: *mut u32,
    ) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
}

/// Owned IOKit object handle (service, iterator); released on drop.
pub(crate) struct IoObject(pub(crate) u32);

impl Drop for IoObject {
    /// Release the kernel reference exactly once.
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { IOObjectRelease(self.0) };
        }
    }
}

impl IoObject {
    /// Every service matching an IOKit class name (the matching dictionary is
    /// consumed by IOKit). Empty on any error.
    pub(crate) fn services(class: &CStr) -> Vec<IoObject> {
        let matching = unsafe { IOServiceMatching(class.as_ptr()) };
        if matching.is_null() {
            return Vec::new();
        }
        let mut iterator = 0;
        if unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0 || iterator == 0
        {
            return Vec::new();
        }
        IoObject(iterator).drain()
    }

    /// First service matching an IOKit class name.
    pub(crate) fn service(class: &CStr) -> Option<IoObject> {
        let matching = unsafe { IOServiceMatching(class.as_ptr()) };
        if matching.is_null() {
            return None;
        }
        let entry = unsafe { IOServiceGetMatchingService(0, matching) };
        (entry != 0).then_some(IoObject(entry))
    }

    /// The IOMedia service for a BSD device name such as "disk3s1s1".
    pub(crate) fn bsd_media(bsd_name: &str) -> Option<IoObject> {
        let name = CString::new(bsd_name).ok()?;
        let matching = unsafe { IOBSDNameMatching(0, 0, name.as_ptr()) };
        if matching.is_null() {
            return None;
        }
        let entry = unsafe { IOServiceGetMatchingService(0, matching) };
        (entry != 0).then_some(IoObject(entry))
    }

    /// Consume an iterator into its remaining entries (bounded).
    fn drain(self) -> Vec<IoObject> {
        let mut out = Vec::new();
        loop {
            let entry = unsafe { IOIteratorNext(self.0) };
            if entry == 0 || out.len() >= 1024 {
                if entry != 0 {
                    unsafe { IOObjectRelease(entry) };
                }
                break;
            }
            out.push(IoObject(entry));
        }
        out
    }

    /// Stable registry id (survives re-enumeration; used as a cache key).
    pub(crate) fn registry_id(&self) -> Option<u64> {
        let mut id = 0;
        (unsafe { IORegistryEntryGetRegistryEntryID(self.0, &mut id) } == 0).then_some(id)
    }

    /// Whether the entry is (a subclass of) the given IOKit class.
    pub(crate) fn conforms_to(&self, class: &CStr) -> bool {
        unsafe { IOObjectConformsTo(self.0, class.as_ptr()) != 0 }
    }

    /// Direct parent in the IOService plane.
    pub(crate) fn parent(&self) -> Option<IoObject> {
        let mut parent = 0;
        let rc =
            unsafe { IORegistryEntryGetParentEntry(self.0, c"IOService".as_ptr(), &mut parent) };
        (rc == 0 && parent != 0).then_some(IoObject(parent))
    }

    /// All direct parents in the IOService plane.
    pub(crate) fn parents(&self) -> Vec<IoObject> {
        let mut iterator = 0;
        let rc = unsafe {
            IORegistryEntryGetParentIterator(self.0, c"IOService".as_ptr(), &mut iterator)
        };
        if rc != 0 || iterator == 0 {
            return Vec::new();
        }
        IoObject(iterator).drain()
    }

    /// One registry property as a retained CF object.
    pub(crate) fn property(&self, key: &str) -> Option<CFType> {
        let key = CFString::new(key);
        let raw = unsafe {
            IORegistryEntryCreateCFProperty(self.0, key.as_concrete_TypeRef(), std::ptr::null(), 0)
        };
        (!raw.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(raw) })
    }

    /// Integer property (CFNumber); signed values keep their sign.
    pub(crate) fn int(&self, key: &str) -> Option<i64> {
        self.property(key)?.downcast::<CFNumber>()?.to_i64()
    }

    /// Boolean property (CFBoolean).
    pub(crate) fn boolean(&self, key: &str) -> Option<bool> {
        let value = self.property(key)?;
        value
            .downcast::<core_foundation::boolean::CFBoolean>()
            .map(bool::from)
    }

    /// String property (CFString).
    pub(crate) fn string(&self, key: &str) -> Option<String> {
        Some(self.property(key)?.downcast::<CFString>()?.to_string())
    }

    /// Raw bytes property (CFData), e.g. IOMACAddress.
    pub(crate) fn data(&self, key: &str) -> Option<Vec<u8>> {
        Some(self.property(key)?.downcast::<CFData>()?.bytes().to_vec())
    }

    /// Dictionary property with string keys, e.g. PerformanceStatistics.
    pub(crate) fn dict(&self, key: &str) -> Option<CFDictionary<CFString, CFType>> {
        let value = self.property(key)?;
        let untyped = value.downcast::<CFDictionary>()?;
        // Re-wrap with typed keys; the retain is taken by wrap_under_get_rule.
        Some(unsafe { CFDictionary::wrap_under_get_rule(untyped.as_concrete_TypeRef()) })
    }
}

/// Integer value of `key` in a CF dictionary (CFNumber), if present.
pub(crate) fn dict_int(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<i64> {
    dict.find(CFString::new(key))?
        .downcast::<CFNumber>()?
        .to_i64()
}

/// `sysctlbyname` into a plain-old-data value; None unless the kernel wrote
/// exactly `size_of::<T>()` bytes (a size mismatch means a different ABI).
pub(crate) fn sysctl_pod<T: Copy>(name: &CStr) -> Option<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && size == std::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

/// Integer sysctl that may be 32- or 64-bit depending on the key.
pub(crate) fn sysctl_int(name: &CStr) -> Option<i64> {
    let mut buf = [0u8; 8];
    let mut size = buf.len();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match size {
        4 => Some(i64::from(i32::from_ne_bytes(buf[..4].try_into().ok()?))),
        8 => Some(i64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// String sysctl such as machdep.cpu.brand_string.
pub(crate) fn sysctl_string(name: &CStr) -> Option<String> {
    let mut size = 0usize;
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 || size > 4096 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    let text = CStr::from_bytes_until_nul(&buf).ok()?.to_string_lossy();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Raw sysctl by MIB (route/interface tables). Retries once when the table
/// grows between the size probe and the read.
pub(crate) fn sysctl_mib(mib: &mut [libc::c_int]) -> Option<Vec<u8>> {
    for _ in 0..2 {
        let mut size = 0usize;
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as u32,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size > 16 * 1024 * 1024 {
            return None;
        }
        // Headroom for entries added between the two calls.
        let mut buf = vec![0u8; size + size / 8 + 512];
        let mut len = buf.len();
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as u32,
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            buf.truncate(len);
            return Some(buf);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
            return None;
        }
    }
    None
}

/// Run a system tool with a hard timeout and output cap; stdout on success.
/// Used only off the hot path (cached health / Wi-Fi fallbacks, ps fallback).
pub(crate) fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    const MAX_OUTPUT: usize = 8 * 1024 * 1024;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("LC_ALL", "C")
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a helper thread so a chatty child cannot block on a full pipe
    // while we poll for exit.
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    (status?.success() && out.len() <= MAX_OUTPUT).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysctls_read_basic_machine_facts() {
        assert!(sysctl_int(c"hw.memsize").unwrap_or(0) > 0);
        assert!(sysctl_int(c"hw.logicalcpu").unwrap_or(0) > 0);
        assert!(sysctl_string(c"kern.ostype").is_some());
        assert!(sysctl_int(c"no.such.key").is_none());
    }

    #[test]
    fn bounded_runner_returns_output_and_enforces_timeout() {
        assert_eq!(
            run_bounded("/bin/echo", &["hi"], Duration::from_secs(5)).as_deref(),
            Some(&b"hi\n"[..])
        );
        let t = Instant::now();
        assert!(run_bounded("/bin/sleep", &["5"], Duration::from_millis(100)).is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(run_bounded("/bin/false", &[], Duration::from_secs(5)).is_none());
    }
}
