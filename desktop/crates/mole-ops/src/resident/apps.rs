// Running-application facts from AppKit and the polite "Quit" action.
//
// quit_app asks a regular (Dock) app to quit exactly like Dock → Quit:
// NSRunningApplication.terminate, which the app may answer with a save
// prompt or refuse. It never signals the process, needs no admin rights and
// triggers no Automation (Apple Events) permission prompt.

use super::objc;
use std::collections::HashMap;

/// NSApplicationActivationPolicyRegular: the app has a Dock icon / menu bar.
const ACTIVATION_POLICY_REGULAR: isize = 0;

/// One running application as LaunchServices sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunningApp {
    pub name: Option<String>,
    pub bundle_path: Option<String>,
    pub regular: bool,
}

/// pid → running-app facts for every NSRunningApplication. Empty when
/// AppKit cannot be loaded (headless contexts).
pub(crate) fn running_apps() -> HashMap<u32, RunningApp> {
    if !objc::appkit() {
        return HashMap::new();
    }
    objc::with_pool(|| unsafe {
        let ws = objc::send_id(objc::class(c"NSWorkspace"), c"sharedWorkspace");
        let apps = objc::send_id(ws, c"runningApplications");
        let count = objc::send_isize(apps, c"count").unwrap_or(0).clamp(0, 4096) as usize;
        let mut out = HashMap::with_capacity(count);
        for i in 0..count {
            let app = objc::send_id_usize(apps, c"objectAtIndex:", i);
            let Some(pid) = objc::send_i32(app, c"processIdentifier").filter(|p| *p > 0) else {
                continue;
            };
            if objc::send_bool(app, c"isTerminated") == Some(true) {
                continue;
            }
            let regular =
                objc::send_isize(app, c"activationPolicy") == Some(ACTIVATION_POLICY_REGULAR);
            let name = objc::nsstring(objc::send_id(app, c"localizedName"))
                .filter(|n| !n.trim().is_empty());
            let bundle_path = objc::url_path(objc::send_id(app, c"bundleURL"));
            out.insert(
                pid as u32,
                RunningApp {
                    name,
                    bundle_path,
                    regular,
                },
            );
        }
        out
    })
}

/// Whether a process with this pid exists (kill(pid, 0) without sending).
fn process_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let rc = unsafe { libc::kill(pid, 0) };
    // EPERM means it exists but belongs to someone else.
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Ask the app owning `pid` to quit normally (like Dock → Quit; never a
/// kill, no admin). Err carries a stable code: "unavailable" (process gone),
/// "unsupported" (not a regular app), "failed".
pub fn quit_app(pid: u32) -> Result<(), String> {
    // kernel_task and launchd are never apps (and kill(0, 0) would probe our
    // own process group instead of a pid).
    if pid <= 1 {
        return Err("unsupported".into());
    }
    if !process_exists(pid) {
        return Err("unavailable".into());
    }
    if !objc::appkit() {
        return Err("unsupported".into());
    }
    objc::with_pool(|| unsafe {
        let app = objc::send_id_i32(
            objc::class(c"NSRunningApplication"),
            c"runningApplicationWithProcessIdentifier:",
            pid as i32,
        );
        if app.is_null() {
            return Err("unsupported".into());
        }
        if objc::send_bool(app, c"isTerminated") == Some(true) {
            return Err("unavailable".into());
        }
        if objc::send_isize(app, c"activationPolicy") != Some(ACTIVATION_POLICY_REGULAR) {
            return Err("unsupported".into());
        }
        match objc::send_bool(app, c"terminate") {
            Some(true) => Ok(()),
            _ => Err("failed".into()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_refuses_missing_and_system_pids() {
        assert_eq!(quit_app(0), Err("unsupported".into()));
        assert_eq!(quit_app(1), Err("unsupported".into()));
        assert_eq!(quit_app(u32::MAX), Err("unavailable".into()));
        // A pid far above pid_max never exists.
        assert_eq!(quit_app(9_999_999), Err("unavailable".into()));
    }

    /// Our own test binary is a process but not a regular app.
    #[test]
    fn quit_refuses_non_app_process() {
        assert_eq!(quit_app(std::process::id()), Err("unsupported".into()));
    }

    #[test]
    fn running_apps_lists_something_on_a_desktop() {
        // Headless CI may have no window server; only check shape.
        for (pid, app) in running_apps() {
            assert!(pid > 0);
            if let Some(name) = app.name {
                assert!(!name.is_empty());
            }
        }
    }
}
