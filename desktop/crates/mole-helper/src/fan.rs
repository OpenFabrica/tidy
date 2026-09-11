// Privileged fan daemon: the only process that ever writes fan keys to the
// SMC. Launched as root by the GUI (one admin prompt), it listens on a Unix
// socket only the invoking user can open and applies `set`/`auto` requests.
//
// Safety contract — fans must never stay forced without a GUI to undo it:
//   * a watchdog thread polls the GUI pid; when it is gone, restore auto, exit
//   * SIGTERM/SIGINT/SIGHUP set a flag the same watchdog acts on
//   * `stop` restores auto before exiting; `auto` restores without exiting
//   * every RPM is re-clamped here into the fan's own [min, max] band
//   * a forced target is re-applied every few seconds so a firmware reset
//     (sleep/wake) cannot silently drop to a stale mode

use mole_ops::fancontrol::{DaemonStatus, Request, SOCKET_PATH};
use mole_ops::smc;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Set by the signal handlers; the watchdog turns it into restore + exit.
static STOP: AtomicBool = AtomicBool::new(false);

/// Currently forced target (None = auto). Shared by the request loop and the
/// re-apply tick.
static TARGET: Mutex<Option<f32>> = Mutex::new(None);

/// Unlink our own socket node at the fixed SOCKET_PATH. This is not a file
/// deletion feature (the workspace routes those through mole_core::sink); it
/// removes only the rendezvous node this daemon itself creates, so it goes
/// straight to unlink(2) with the constant path rather than std::fs.
fn unlink_socket() {
    if let Ok(c_path) = CString::new(SOCKET_PATH) {
        unsafe { libc::unlink(c_path.as_ptr()) };
    }
}

/// Pid liveness via the zero signal (root may signal anyone, so a false
/// "alive" for a foreign process is impossible; ESRCH means gone).
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

extern "C" fn on_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Restore auto with retries: this is the one operation that must not fail
/// quietly, so log and keep trying a few times before giving up.
fn restore_auto() {
    for attempt in 1..=3 {
        match smc::set_fans_auto() {
            Ok(()) => {
                *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = None;
                return;
            }
            Err(e) => eprintln!("fan-daemon: restore auto failed (try {attempt}): {e}"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Serve one request line and write the reply.
fn handle(stream: &mut UnixStream) {
    let timeout = Some(Duration::from_secs(2));
    stream.set_read_timeout(timeout).ok();
    stream.set_write_timeout(timeout).ok();
    let mut line = String::new();
    if BufReader::new(&*stream).read_line(&mut line).is_err() {
        return;
    }
    let reply = match Request::parse(&line) {
        None => "err bad request".to_string(),
        Some(Request::Status) => {
            let target = *TARGET.lock().unwrap_or_else(|e| e.into_inner());
            let st = DaemonStatus {
                forced: smc::fans_forced().unwrap_or(target.is_some()),
                target_rpm: target,
            };
            format!("ok {}", st.to_wire())
        }
        Some(Request::Set(rpm)) => match smc::set_fans_forced(rpm) {
            Ok(applied) => {
                *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = Some(applied);
                format!("ok {applied}")
            }
            Err(e) => format!("err {e}"),
        },
        Some(Request::Auto) => {
            restore_auto();
            match smc::fans_forced() {
                Some(true) => "err fans still forced".to_string(),
                _ => "ok".to_string(),
            }
        }
        Some(Request::Stop) => {
            restore_auto();
            let _ = stream.write_all(b"ok\n");
            STOP.store(true, Ordering::SeqCst);
            return;
        }
    };
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
}

/// Bind the socket and lock it down to the invoking user (0600, chown uid)
/// so no other local account can drive the fans through us.
fn bind_socket(uid: u32) -> Result<UnixListener, String> {
    unlink_socket();
    let listener =
        UnixListener::bind(SOCKET_PATH).map_err(|e| format!("bind {SOCKET_PATH}: {e}"))?;
    let c_path = CString::new(SOCKET_PATH).map_err(|e| e.to_string())?;
    let rc = unsafe { libc::chmod(c_path.as_ptr(), 0o600) };
    if rc != 0 {
        return Err("chmod socket failed".into());
    }
    // gid -1 keeps the group untouched; only the owner matters for 0600.
    let rc = unsafe { libc::chown(c_path.as_ptr(), uid, u32::MAX) };
    if rc != 0 {
        return Err("chown socket failed".into());
    }
    // Short accept timeout via non-blocking + poll loop keeps the request
    // loop responsive to STOP without a second listener thread.
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(listener)
}

/// Daemon main loop. Returns only on a startup error; a normal life ends in
/// `process::exit` from the watchdog (after restoring auto).
pub fn run(gui_pid: i32, uid: u32) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("fan-daemon must run as root".into());
    }
    if !pid_alive(gui_pid) {
        return Err(format!("gui pid {gui_pid} is not running"));
    }
    if mole_ops::fancontrol::is_running() {
        return Err("another fan-daemon is already serving".into());
    }
    let listener = bind_socket(uid)?;
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGHUP, on_signal as *const () as usize);
        // Writing to a peer that vanished must not kill us mid-restore.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // Watchdog: GUI death or a signal → restore auto, drop the socket, exit.
    std::thread::spawn(move || loop {
        if STOP.load(Ordering::SeqCst) || !pid_alive(gui_pid) {
            restore_auto();
            unlink_socket();
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(500));
    });

    let mut last_reapply = Instant::now();
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                handle(&mut stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("fan-daemon: accept: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        // Re-assert a forced target periodically (firmware may reset mode).
        if last_reapply.elapsed() >= Duration::from_secs(5) {
            last_reapply = Instant::now();
            let target = *TARGET.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rpm) = target {
                if let Err(e) = smc::set_fans_forced(rpm) {
                    eprintln!("fan-daemon: re-apply {rpm}: {e}");
                }
            }
        }
    }
}
