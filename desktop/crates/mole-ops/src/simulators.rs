// Simulators: CoreSimulator devices reported by `xcrun simctl list devices
// --json`. Nothing here touches the filesystem; removal goes through
// `simctl delete` (docker.rs pattern). Deleting `Devices/<UDID>` by hand is
// not an option: CoreSimulatorService keeps the device set in memory, so the
// device lingers as a ghost in `simctl list` / Xcode until the service
// restarts. Only Shutdown devices are ever offered, and the state is checked
// again right before each delete.

use crate::optimize::{run_bounded, CommandStatus};
use crate::scanutil::{self, CancelFlag};
use mole_core::plan::SizeKb;
use mole_core::probes::{LiveProbes, TriState};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

const XCRUN: &str = "/usr/bin/xcrun";
const RUNTIME_PREFIX: &str = "com.apple.CoreSimulator.SimRuntime.";
/// The only device state that may be deleted.
const SHUTDOWN: &str = "Shutdown";
/// Process name of Simulator.app; while it runs it may boot a device at any
/// moment, so nothing is offered or deleted.
const SIMULATOR_APP: &str = "Simulator";

/// One Shutdown simulator device as shown in the preview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SimDevice {
    /// Device UDID (the removal key), validated to UUID shape.
    pub udid: String,
    pub name: String,
    /// Human-readable runtime, e.g. "iOS 26.3".
    pub runtime: String,
    pub state: String,
    /// False when the device's runtime is no longer installed.
    pub is_available: bool,
    /// Allocated size of `Devices/<UDID>`; None when it could not be walked.
    pub size_kb: Option<u64>,
    /// Days since last boot (`lastBootedAt`), else since the device dir changed.
    pub age_days: Option<u64>,
}

/// Why the simulator section cannot be offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// No Xcode (simctl ships only with Xcode, not the Command Line Tools).
    NoXcode,
    /// Simulator.app is running, or that could not be ruled out.
    SimulatorRunning,
    /// simctl failed or printed something we do not understand.
    ListFailed,
    Cancelled,
}

impl Unavailable {
    /// Short machine-readable reason for IPC errors.
    pub fn reason(self) -> &'static str {
        match self {
            Unavailable::NoXcode => "xcode not installed",
            Unavailable::SimulatorRunning => "simulator running",
            Unavailable::ListFailed => "simctl list failed",
            Unavailable::Cancelled => "scan cancelled",
        }
    }
}

/// One device row straight from simctl JSON, before sizing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawDevice {
    udid: String,
    name: String,
    runtime: String,
    state: String,
    is_available: bool,
    data_path: Option<String>,
    last_booted_at: Option<String>,
}

/// Strict 8-4-4-4-12 hex UUID shape check: the UDID is the only value from
/// simctl output that reaches a command line.
pub fn is_valid_udid(udid: &str) -> bool {
    let bytes = udid.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// "com.apple.CoreSimulator.SimRuntime.iOS-26-3" → "iOS 26.3" (unknown shapes
/// are returned as-is rather than guessed at).
fn runtime_label(key: &str) -> String {
    let Some(rest) = key.strip_prefix(RUNTIME_PREFIX) else {
        return key.to_string();
    };
    let mut parts = rest.split('-');
    let platform = match parts.next() {
        Some("xrOS") => "visionOS",
        Some(p) if !p.is_empty() => p,
        _ => return key.to_string(),
    };
    let version: Vec<&str> = parts.collect();
    if version.is_empty() {
        platform.to_string()
    } else {
        format!("{platform} {}", version.join("."))
    }
}

/// Parse `simctl list devices --json` into rows with a well-formed UDID.
/// None when the JSON is not understood (no guessing).
fn parse_devices(json: &str) -> Option<Vec<RawDevice>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let devices = value.get("devices")?.as_object()?;
    let str_of = |o: &serde_json::Value, k: &str| o.get(k)?.as_str().map(str::to_string);
    let mut out = Vec::new();
    for (runtime, list) in devices {
        let Some(list) = list.as_array() else {
            continue;
        };
        for dev in list {
            let Some(udid) = str_of(dev, "udid").filter(|u| is_valid_udid(u)) else {
                continue;
            };
            out.push(RawDevice {
                udid,
                name: str_of(dev, "name").unwrap_or_else(|| "Simulator".into()),
                runtime: runtime_label(runtime),
                state: str_of(dev, "state").unwrap_or_default(),
                // Missing flag counts as available; only an explicit false
                // marks the runtime as gone.
                is_available: dev.get("isAvailable").and_then(|v| v.as_bool()) != Some(false),
                data_path: str_of(dev, "dataPath"),
                last_booted_at: str_of(dev, "lastBootedAt"),
            });
        }
    }
    Some(out)
}

/// Keep only devices that may be deleted: state exactly Shutdown. Booted,
/// Booting, Shutting Down, Creating and unknown states are all refused.
fn deletable(devices: Vec<RawDevice>) -> Vec<RawDevice> {
    devices
        .into_iter()
        .filter(|d| d.state == SHUTDOWN)
        .collect()
}

/// Current state of one device in a fresh listing; None when it is gone.
fn state_of(json: &str, udid: &str) -> Option<String> {
    parse_devices(json)?
        .into_iter()
        .find(|d| d.udid == udid)
        .map(|d| d.state)
}

/// Unavailable devices first (dead weight), then largest first.
fn sort_for_preview(devices: &mut [SimDevice]) {
    devices.sort_by(|a, b| {
        a.is_available
            .cmp(&b.is_available)
            .then(b.size_kb.unwrap_or(0).cmp(&a.size_kb.unwrap_or(0)))
    });
}

/// Days from an RFC 3339 stamp ("2026-09-12T16:40:56Z") to now.
fn days_since_stamp(stamp: &str) -> Option<u64> {
    let t = chrono::DateTime::parse_from_rfc3339(stamp).ok()?;
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
    Some(secs.max(0) as u64 / 86_400)
}

/// Days since a directory's mtime (fallback for never-booted devices).
fn days_since_mtime(dir: &Path) -> Option<u64> {
    let modified = std::fs::metadata(dir).ok()?.modified().ok()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    Some(age.as_secs() / 86_400)
}

/// The device dir `Devices/<UDID>`: dataPath's parent when it really is that
/// device's dir, else the default device set under the home folder.
fn device_dir(home: &str, dev: &RawDevice) -> PathBuf {
    if let Some(parent) = dev.data_path.as_deref().and_then(|p| Path::new(p).parent()) {
        if parent.file_name().and_then(|n| n.to_str()) == Some(dev.udid.as_str()) {
            return parent.to_path_buf();
        }
    }
    Path::new(home)
        .join("Library/Developer/CoreSimulator/Devices")
        .join(&dev.udid)
}

/// True when an Xcode developer dir with simctl is selected. Checked before
/// any `xcrun simctl` call because the bare xcrun shim on a machine without
/// Xcode can pop the "install developer tools" dialog.
fn has_simctl() -> bool {
    let argv = ["/usr/bin/xcode-select".to_string(), "-p".into()];
    let result = run_bounded(&argv, Duration::from_secs(10));
    result.success()
        && Path::new(result.output.trim())
            .join("usr/bin/simctl")
            .is_file()
}

/// Run `xcrun simctl list devices --json` and return its raw output.
fn list_json() -> Result<String, Unavailable> {
    let argv = [
        XCRUN.to_string(),
        "simctl".into(),
        "list".into(),
        "devices".into(),
        "--json".into(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(30));
    if result.status != CommandStatus::Success {
        return Err(Unavailable::ListFailed);
    }
    Ok(result.output)
}

/// Refuse while Simulator.app runs; Unknown denies like the rest of the
/// codebase (no proof of idleness, no deletion).
fn ensure_simulator_idle(probes: &dyn LiveProbes) -> Result<(), Unavailable> {
    match probes.any_process_running(&[SIMULATOR_APP]) {
        TriState::Idle => Ok(()),
        _ => Err(Unavailable::SimulatorRunning),
    }
}

/// Scan Shutdown simulator devices. Err when there is no Xcode, Simulator.app
/// is running, or the listing is not understood — the preview then simply has
/// no simulator section (same as docker not running).
pub fn scan(
    home: &str,
    probes: &dyn LiveProbes,
    cancel: &CancelFlag,
) -> Result<Vec<SimDevice>, Unavailable> {
    if !has_simctl() {
        return Err(Unavailable::NoXcode);
    }
    ensure_simulator_idle(probes)?;
    let raw = parse_devices(&list_json()?).ok_or(Unavailable::ListFailed)?;
    let mut devices = Vec::new();
    for dev in deletable(raw) {
        if cancel.load(Ordering::Relaxed) {
            return Err(Unavailable::Cancelled);
        }
        let dir = device_dir(home, &dev);
        let size_kb = if dir.is_dir() {
            match scanutil::dir_size_kb(&dir, cancel) {
                Ok((SizeKb::Known(kb), _)) => Some(kb),
                Ok(_) => None,
                Err(_) => return Err(Unavailable::Cancelled),
            }
        } else {
            None
        };
        let age_days = dev
            .last_booted_at
            .as_deref()
            .and_then(days_since_stamp)
            .or_else(|| days_since_mtime(&dir));
        devices.push(SimDevice {
            udid: dev.udid,
            name: dev.name,
            runtime: dev.runtime,
            state: dev.state,
            is_available: dev.is_available,
            size_kb,
            age_days,
        });
    }
    sort_for_preview(&mut devices);
    Ok(devices)
}

/// Delete one device through `simctl delete`. The UDID is re-validated and
/// the device must still be Shutdown in a fresh listing (it may have been
/// booted since the preview) with Simulator.app not running.
pub fn delete(udid: &str, probes: &dyn LiveProbes) -> Result<(), String> {
    if !is_valid_udid(udid) {
        return Err("invalid simulator udid".into());
    }
    ensure_simulator_idle(probes).map_err(|e| e.reason().to_string())?;
    let json = list_json().map_err(|e| e.reason().to_string())?;
    match state_of(&json, udid) {
        None => return Err("simulator no longer exists".into()),
        Some(state) if state != SHUTDOWN => {
            return Err(format!("simulator is {state}, not {SHUTDOWN}"));
        }
        Some(_) => {}
    }
    let argv = [
        XCRUN.to_string(),
        "simctl".into(),
        "delete".into(),
        udid.to_string(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(120));
    if result.success() {
        Ok(())
    } else {
        Err(result.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mole_core::probes::StubProbes;

    const FIXTURE: &str = r#"{"devices": {
      "com.apple.CoreSimulator.SimRuntime.iOS-26-3": [
        {"udid":"C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA","name":"iPhone 17 Pro","isAvailable":true,"state":"Shutdown",
         "lastBootedAt":"2020-01-01T00:00:00Z",
         "dataPath":"/Users/x/Library/Developer/CoreSimulator/Devices/C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA/data"},
        {"udid":"11111111-2222-3333-4444-555555555555","name":"iPhone Air","isAvailable":true,"state":"Booted"},
        {"udid":"not-a-udid","name":"Broken","isAvailable":true,"state":"Shutdown"}
      ],
      "com.apple.CoreSimulator.SimRuntime.iOS-16-4": [
        {"udid":"A1B2C3D4-E5F6-7890-ABCD-EF0123456789","name":"iPhone 14","isAvailable":false,"state":"Shutdown"}
      ],
      "com.apple.CoreSimulator.SimRuntime.xrOS-2-0": [
        {"udid":"22222222-2222-3333-4444-555555555555","name":"Vision","state":"Shutting Down"}
      ]
    }}"#;

    /// Look up a parsed fixture row by UDID.
    fn find<'a>(devices: &'a [RawDevice], udid: &str) -> &'a RawDevice {
        devices.iter().find(|d| d.udid == udid).unwrap()
    }

    #[test]
    fn udid_shape_is_strict() {
        assert!(is_valid_udid("A1B2C3D4-E5F6-7890-ABCD-EF0123456789"));
        assert!(is_valid_udid("a1b2c3d4-e5f6-7890-abcd-ef0123456789"));
        assert!(!is_valid_udid("A1B2C3D4-E5F6-7890-ABCD-EF012345678"));
        assert!(!is_valid_udid("A1B2C3D4-E5F6-7890-ABCD-EF012345678G"));
        assert!(!is_valid_udid("A1B2C3D4E5F6-7890-ABCD-EF0123456789-"));
        assert!(!is_valid_udid("; rm -rf /"));
    }

    #[test]
    fn runtime_keys_become_readable() {
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-26-3"),
            "iOS 26.3"
        );
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.watchOS-11-0"),
            "watchOS 11.0"
        );
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.xrOS-2-0"),
            "visionOS 2.0"
        );
        assert_eq!(runtime_label("custom.runtime"), "custom.runtime");
    }

    #[test]
    fn simctl_json_parses_with_fields_and_drops_bad_udids() {
        let devices = parse_devices(FIXTURE).unwrap();
        assert_eq!(devices.len(), 4, "malformed udid must be dropped");
        let pro = find(&devices, "C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA");
        assert_eq!(pro.name, "iPhone 17 Pro");
        assert_eq!(pro.runtime, "iOS 26.3");
        assert!(pro.is_available);
        assert!(pro.data_path.is_some());
        assert!(days_since_stamp(pro.last_booted_at.as_deref().unwrap()).unwrap() > 365 * 5);
        let old = find(&devices, "A1B2C3D4-E5F6-7890-ABCD-EF0123456789");
        assert!(!old.is_available);
        assert_eq!(old.runtime, "iOS 16.4");
        // Missing isAvailable counts as available.
        assert!(find(&devices, "22222222-2222-3333-4444-555555555555").is_available);
        assert!(parse_devices("not json").is_none());
        assert!(parse_devices(r#"{"devices": []}"#).is_none());
    }

    #[test]
    fn only_shutdown_devices_are_offered() {
        let offered = deletable(parse_devices(FIXTURE).unwrap());
        let mut udids: Vec<&str> = offered.iter().map(|d| d.udid.as_str()).collect();
        udids.sort();
        assert_eq!(
            udids,
            [
                "A1B2C3D4-E5F6-7890-ABCD-EF0123456789",
                "C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA"
            ]
        );
    }

    #[test]
    fn execution_recheck_reads_fresh_state() {
        assert_eq!(
            state_of(FIXTURE, "11111111-2222-3333-4444-555555555555").as_deref(),
            Some("Booted")
        );
        assert_eq!(
            state_of(FIXTURE, "A1B2C3D4-E5F6-7890-ABCD-EF0123456789").as_deref(),
            Some("Shutdown")
        );
        assert_eq!(
            state_of(FIXTURE, "33333333-2222-3333-4444-555555555555"),
            None
        );
    }

    #[test]
    fn device_dir_trusts_data_path_only_when_it_names_the_device() {
        let devices = parse_devices(FIXTURE).unwrap();
        let pro = find(&devices, "C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA");
        assert_eq!(
            device_dir("/h", pro),
            PathBuf::from(
                "/Users/x/Library/Developer/CoreSimulator/Devices/C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA"
            )
        );
        let mut spoofed = pro.clone();
        spoofed.data_path = Some("/elsewhere/data".into());
        assert_eq!(
            device_dir("/h", &spoofed),
            PathBuf::from(
                "/h/Library/Developer/CoreSimulator/Devices/C8448ADC-F3B0-40B7-A83B-EE0D02C06EEA"
            )
        );
    }

    #[test]
    fn preview_puts_unavailable_first_then_largest() {
        let dev = |udid: &str, avail: bool, kb: u64| SimDevice {
            udid: udid.into(),
            name: String::new(),
            runtime: String::new(),
            state: SHUTDOWN.into(),
            is_available: avail,
            size_kb: Some(kb),
            age_days: None,
        };
        let mut list = vec![dev("a", true, 10), dev("b", false, 1), dev("c", true, 99)];
        sort_for_preview(&mut list);
        let order: Vec<&str> = list.iter().map(|d| d.udid.as_str()).collect();
        assert_eq!(order, ["b", "c", "a"]);
    }

    #[test]
    fn unknown_or_running_simulator_app_is_refused() {
        assert!(ensure_simulator_idle(&StubProbes::idle()).is_ok());
        let unknown = StubProbes {
            owner_state: TriState::Unknown,
            sqlite_state: TriState::Idle,
        };
        assert_eq!(
            ensure_simulator_idle(&unknown),
            Err(Unavailable::SimulatorRunning)
        );
        let active = StubProbes {
            owner_state: TriState::Active,
            sqlite_state: TriState::Idle,
        };
        assert_eq!(
            ensure_simulator_idle(&active),
            Err(Unavailable::SimulatorRunning)
        );
    }

    #[test]
    fn delete_rejects_malformed_udid_before_any_command() {
        assert_eq!(
            delete("$(id)", &StubProbes::idle()).unwrap_err(),
            "invalid simulator udid"
        );
    }
}

#[cfg(test)]
mod real_machine {
    /// Manual, read-only: cargo test -p mole-ops real_simulator_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_simulator_scan() {
        let home = std::env::var("HOME").unwrap();
        let probes = mole_core::probes::SystemProbes::new();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        match super::scan(&home, &probes, &cancel) {
            Ok(devices) => {
                for d in devices {
                    eprintln!("{:?}", d);
                }
            }
            Err(e) => eprintln!("unavailable: {}", e.reason()),
        }
    }
}
