// Minimal Objective-C runtime bridge for the two AppKit/CoreWLAN reads the
// resident feature needs (running applications, Wi-Fi PHY/channel) and the
// polite NSRunningApplication.terminate. Frameworks are dlopen'ed on first use
// instead of linked, so the privileged mole-helper (which links mole-ops)
// never loads AppKit. Every call runs inside an autorelease pool.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

pub(crate) type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// dlopen a system framework; RTLD_NOLOAD first because inside the Tauri
/// app AppKit is already mapped.
fn open_framework(path: &CStr) -> bool {
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOLOAD | libc::RTLD_LAZY) };
    !handle.is_null() || !unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) }.is_null()
}

/// AppKit loaded (once per process); false when it cannot load.
pub(crate) fn appkit() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| open_framework(c"/System/Library/Frameworks/AppKit.framework/AppKit"))
}

/// CoreWLAN loaded (once per process); false when it cannot load.
pub(crate) fn corewlan() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED
        .get_or_init(|| open_framework(c"/System/Library/Frameworks/CoreWLAN.framework/CoreWLAN"))
}

/// Run `f` inside an autorelease pool so autoreleased returns are freed.
pub(crate) fn with_pool<R>(f: impl FnOnce() -> R) -> R {
    struct Pool(*mut c_void);
    impl Drop for Pool {
        /// Pop even if `f` panics.
        fn drop(&mut self) {
            unsafe { objc_autoreleasePoolPop(self.0) };
        }
    }
    let _pool = Pool(unsafe { objc_autoreleasePoolPush() });
    f()
}

/// Class object by name; null when the framework is not loaded.
pub(crate) fn class(name: &CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

/// Selector by name.
fn sel(name: &CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

// Typed objc_msgSend trampolines. On arm64 and x86_64 a non-variadic cast of
// objc_msgSend to the exact method signature is the documented calling
// convention for methods returning integers, pointers and BOOL.

/// `[obj sel]` returning an object.
pub(crate) unsafe fn send_id(obj: Id, name: &CStr) -> Id {
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(name))
}

/// `[obj sel:int]` returning an object (pid_t argument).
pub(crate) unsafe fn send_id_i32(obj: Id, name: &CStr, arg: i32) -> Id {
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    let f: unsafe extern "C" fn(Id, Sel, i32) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(name), arg)
}

/// `[obj sel:NSUInteger]` returning an object (objectAtIndex:).
pub(crate) unsafe fn send_id_usize(obj: Id, name: &CStr, arg: usize) -> Id {
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    let f: unsafe extern "C" fn(Id, Sel, usize) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(name), arg)
}

/// `[obj sel:ptr]` returning an object (stringWithUTF8String:, interfaceWithName:).
pub(crate) unsafe fn send_id_ptr(obj: Id, name: &CStr, arg: *const c_void) -> Id {
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    let f: unsafe extern "C" fn(Id, Sel, *const c_void) -> Id =
        std::mem::transmute(objc_msgSend as *const ());
    f(obj, sel(name), arg)
}

/// `[obj sel]` returning NSInteger / NSUInteger / pid_t widened to isize.
pub(crate) unsafe fn send_isize(obj: Id, name: &CStr) -> Option<isize> {
    if obj.is_null() {
        return None;
    }
    let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc_msgSend as *const ());
    Some(f(obj, sel(name)))
}

/// `[obj sel]` returning pid_t (int).
pub(crate) unsafe fn send_i32(obj: Id, name: &CStr) -> Option<i32> {
    if obj.is_null() {
        return None;
    }
    let f: unsafe extern "C" fn(Id, Sel) -> i32 = std::mem::transmute(objc_msgSend as *const ());
    Some(f(obj, sel(name)))
}

/// `[obj sel]` returning BOOL.
pub(crate) unsafe fn send_bool(obj: Id, name: &CStr) -> Option<bool> {
    if obj.is_null() {
        return None;
    }
    // BOOL is `bool` on arm64 and `signed char` on x86_64; both are one byte
    // returned in the low bits of the first return register.
    let f: unsafe extern "C" fn(Id, Sel) -> i8 = std::mem::transmute(objc_msgSend as *const ());
    Some(f(obj, sel(name)) != 0)
}

/// NSString → Rust String (UTF-8 copy).
pub(crate) unsafe fn nsstring(obj: Id) -> Option<String> {
    let ptr = send_id(obj, c"UTF8String") as *const c_char;
    if ptr.is_null() {
        return None;
    }
    Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
}

/// Rust str → autoreleased NSString.
pub(crate) unsafe fn nsstring_from(text: &str) -> Id {
    let Ok(c) = CString::new(text) else {
        return std::ptr::null_mut();
    };
    send_id_ptr(
        class(c"NSString"),
        c"stringWithUTF8String:",
        c.as_ptr().cast(),
    )
}

/// NSURL → filesystem path.
pub(crate) unsafe fn url_path(url: Id) -> Option<String> {
    nsstring(send_id(url, c"path"))
}
