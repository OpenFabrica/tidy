// Fan control client + wire protocol shared with the privileged fan daemon
// (`mole-helper fan-daemon`). The daemon is the only process that writes the
// SMC; this side just launches it (one admin prompt per session) and talks to
// it over a Unix socket that only the invoking user can open. The daemon
// restores automatic fan control on its own whenever the GUI dies, so nothing
// here is load-bearing for safety — it is a convenience layer.

use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// Socket the daemon listens on. /var/run is root-owned, so nobody can plant
/// a fake socket there before the daemon does.
pub const SOCKET_PATH: &str = "/var/run/tidy-fan.sock";

/// One request line from GUI to daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Force every fan to this RPM (daemon clamps into each fan's band).
    Set(f32),
    /// Hand fans back to macOS.
    Auto,
    /// Report forced/target state.
    Status,
    /// Restore auto and exit.
    Stop,
}

impl Request {
    /// Wire form, newline-terminated.
    pub fn to_line(&self) -> String {
        match self {
            Request::Set(rpm) => format!("set {rpm}\n"),
            Request::Auto => "auto\n".into(),
            Request::Status => "status\n".into(),
            Request::Stop => "stop\n".into(),
        }
    }

    /// Parse one request line; anything unexpected is None (daemon replies err).
    pub fn parse(line: &str) -> Option<Request> {
        let mut parts = line.split_whitespace();
        match (parts.next()?, parts.next(), parts.next()) {
            ("set", Some(v), None) => v
                .parse::<f32>()
                .ok()
                .filter(|r| r.is_finite())
                .map(Request::Set),
            ("auto", None, None) => Some(Request::Auto),
            ("status", None, None) => Some(Request::Status),
            ("stop", None, None) => Some(Request::Stop),
            _ => None,
        }
    }
}

/// Daemon-side fan state as reported by `status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DaemonStatus {
    pub forced: bool,
    pub target_rpm: Option<f32>,
}

impl DaemonStatus {
    /// Wire form after the `ok ` prefix: `forced=<0|1> target=<rpm|->`.
    pub fn to_wire(&self) -> String {
        format!(
            "forced={} target={}",
            u8::from(self.forced),
            self.target_rpm.map_or("-".to_string(), |t| t.to_string())
        )
    }

    /// Inverse of `to_wire`.
    pub fn parse(wire: &str) -> Option<DaemonStatus> {
        let mut forced = None;
        let mut target = None;
        for kv in wire.split_whitespace() {
            match kv.split_once('=')? {
                ("forced", v) => forced = Some(v == "1"),
                ("target", "-") => target = Some(None),
                ("target", v) => target = Some(Some(v.parse::<f32>().ok()?)),
                _ => return None,
            }
        }
        Some(DaemonStatus {
            forced: forced?,
            target_rpm: target?,
        })
    }
}

/// One request/response round-trip. Err carries either the transport failure
/// or the daemon's own `err ...` message, without the prefix.
fn send(req: &Request) -> Result<String, String> {
    let mut stream =
        UnixStream::connect(SOCKET_PATH).map_err(|e| format!("daemon not running: {e}"))?;
    let timeout = Some(Duration::from_secs(3));
    stream.set_read_timeout(timeout).ok();
    stream.set_write_timeout(timeout).ok();
    stream
        .write_all(req.to_line().as_bytes())
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let line = line.trim();
    match line.split_once(' ') {
        Some(("ok", rest)) => Ok(rest.to_string()),
        Some(("err", rest)) => Err(rest.to_string()),
        None if line == "ok" => Ok(String::new()),
        _ => Err(format!("bad reply: {line}")),
    }
}

/// Whether a daemon answers on the socket right now.
pub fn is_running() -> bool {
    status().is_some()
}

/// Daemon state, None when it is not running.
pub fn status() -> Option<DaemonStatus> {
    send(&Request::Status)
        .ok()
        .and_then(|s| DaemonStatus::parse(&s))
}

/// Force all fans to `rpm`; returns the RPM the daemon actually applied.
pub fn set_rpm(rpm: f32) -> Result<f32, String> {
    let reply = send(&Request::Set(rpm))?;
    reply
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("bad reply: {reply}"))
}

/// Hand fans back to macOS (daemon keeps running).
pub fn set_auto() -> Result<(), String> {
    send(&Request::Auto).map(|_| ())
}

/// Restore auto and shut the daemon down. Ok when it was not running. Waits
/// (briefly) for the socket to disappear so a status read right after this
/// call already reports "not running" instead of the exiting daemon.
pub fn stop() -> Result<(), String> {
    match send(&Request::Stop) {
        Ok(_) => {}
        Err(e) if e.starts_with("daemon not running") => return Ok(()),
        Err(e) => return Err(e),
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && Path::new(SOCKET_PATH).exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Launch the daemon as root through the native admin prompt, then wait for
/// its socket. `helper` is the mole-helper binary next to the app binary;
/// only an absolute, existing path is accepted so the elevated command can
/// never be pointed somewhere else by a relative-path trick.
pub fn start(helper: &Path) -> Result<(), String> {
    if is_running() {
        return Ok(());
    }
    if !helper.is_absolute() || !helper.is_file() {
        return Err(format!("helper binary missing: {}", helper.display()));
    }
    let uid = unsafe { libc::getuid() };
    let gui_pid = std::process::id();
    // The daemon detaches (redirects + `&`) so osascript returns as soon as
    // the shell does; the daemon's own watchdog ties its life to gui_pid.
    let cmd = format!(
        "{} fan-daemon --gui-pid {gui_pid} --uid {uid} </dev/null >/dev/null 2>&1 &",
        mole_macos::applescript_shell_quoted(helper)
    );
    mole_macos::run_as_admin(&cmd).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            "cancelled".to_string()
        } else {
            e.to_string()
        }
    })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if is_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("daemon did not start".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines_roundtrip() {
        for req in [
            Request::Set(2500.5),
            Request::Auto,
            Request::Status,
            Request::Stop,
        ] {
            assert_eq!(Request::parse(&req.to_line()), Some(req));
        }
        assert_eq!(Request::parse("set"), None);
        assert_eq!(Request::parse("set nan"), None);
        assert_eq!(Request::parse("set 1 2"), None);
        assert_eq!(Request::parse("reboot"), None);
    }

    #[test]
    fn status_wire_roundtrips() {
        for st in [
            DaemonStatus {
                forced: true,
                target_rpm: Some(3000.0),
            },
            DaemonStatus {
                forced: false,
                target_rpm: None,
            },
        ] {
            assert_eq!(DaemonStatus::parse(&st.to_wire()), Some(st));
        }
        assert_eq!(DaemonStatus::parse("forced=1"), None);
    }

    /// Without a daemon every client call fails closed instead of hanging.
    #[test]
    fn client_fails_fast_without_daemon() {
        if is_running() {
            return;
        }
        assert!(status().is_none());
        assert!(set_rpm(3000.0).is_err());
        assert_eq!(stop(), Ok(()));
    }

    #[test]
    fn start_rejects_relative_or_missing_helper() {
        if is_running() {
            return;
        }
        assert!(start(Path::new("mole-helper")).is_err());
        assert!(start(Path::new("/nonexistent/mole-helper")).is_err());
    }
}
