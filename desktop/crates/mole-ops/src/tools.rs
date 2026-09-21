// Tools: reclaimable items that no path deletion can express and that must
// go through their owning CLI (docker.rs pattern). Time Machine local
// snapshots (`tmutil`, root) and Homebrew's stale downloads/kegs
// (`brew cleanup`). Simulator devices have their own section
// (simulators.rs). Nothing here touches the filesystem directly; each tool
// keeps its own consistency guarantees.
// Every identifier that reaches a command line is validated to a strict
// shape at scan time AND again at execution time.

use crate::optimize::{run_bounded, CommandStatus};
use crate::scanutil::CancelFlag;
use mole_core::probes::{LiveProbes, TriState};
use mole_core::providers::PrivilegedRunner;
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

/// What one tool item removes. Ids come only from the last scan.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolTarget {
    /// Time Machine local snapshot, keyed by its `YYYY-MM-DD-HHMMSS` stamp.
    Snapshot { stamp: String },
    /// `brew cleanup --prune=all`.
    BrewCleanup,
}

/// One reclaimable tool-managed item as shown in the preview.
#[derive(Debug, Clone, Serialize)]
pub struct ToolItem {
    /// Stable id for this scan: "snap:<stamp>", "brew:cleanup".
    pub id: String,
    /// Display label (raw, UI localizes around it): snapshot stamp,
    /// "Homebrew".
    pub label: String,
    /// Extra one-line detail for the UI badge (e.g. "N items"); None if
    /// nothing.
    pub detail: Option<String>,
    pub size_kb: Option<u64>,
    /// True when execution needs admin (snapshots).
    pub requires_admin: bool,
    pub target: ToolTarget,
}

const SNAPSHOT_PREFIX: &str = "com.apple.TimeMachine.";
const SNAPSHOT_SUFFIX: &str = ".local";

/// Strict `YYYY-MM-DD-HHMMSS` shape check: digits and dashes only, fixed
/// positions. Anything else never reaches a shell.
pub fn is_valid_snapshot_stamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    if bytes.len() != 17 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        4 | 7 | 10 => *b == b'-',
        _ => b.is_ascii_digit(),
    })
}

/// Extract snapshot stamps from `tmutil listlocalsnapshots` output; lines
/// that are not exactly `com.apple.TimeMachine.<stamp>.local` are dropped.
fn parse_snapshots(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(SNAPSHOT_PREFIX)?;
            let stamp = rest.strip_suffix(SNAPSHOT_SUFFIX)?;
            is_valid_snapshot_stamp(stamp).then(|| stamp.to_string())
        })
        .collect()
}

/// Parse "This operation would free approximately 1.2GB of disk space." into
/// KB (1024-based, matching docker.rs). None when the line is absent.
fn parse_brew_free_kb(output: &str) -> Option<u64> {
    let marker = "would free approximately ";
    let line = output.lines().find(|l| l.contains(marker))?;
    let rest = &line[line.find(marker)? + marker.len()..];
    let token = rest.split_whitespace().next()?;
    let digits_end = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let value: f64 = token[..digits_end].parse().ok()?;
    let unit = token[digits_end..]
        .trim_end_matches('.')
        .to_ascii_uppercase();
    let multiplier: f64 = match unit.as_str() {
        "B" => 1.0 / 1024.0,
        "KB" => 1.0,
        "MB" => 1024.0,
        "GB" => 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * multiplier).round() as u64)
}

/// Count the "Would remove:" lines of a brew dry run.
fn count_brew_removals(output: &str) -> usize {
    output
        .lines()
        .filter(|l| l.trim_start().starts_with("Would remove"))
        .count()
}

/// Locate the Homebrew binary (Apple Silicon prefix first).
fn find_brew() -> Option<PathBuf> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Environment pinned for every brew call: no auto-update (it would run git
/// fetch inside a cleanup preview) and no analytics.
const BREW_ENV: &[(&str, &str)] = &[
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
];

/// Bounded argv execution with extra environment variables. Same contract as
/// optimize::run_bounded (no shell, pipes drained while waiting, timeout
/// kills the child) — duplicated because run_bounded has no env hook.
fn run_bounded_env(argv: &[String], env: &[(&str, &str)], timeout: Duration) -> (bool, String) {
    let Some((program, arguments)) = argv.split_first() else {
        return (false, "empty command".into());
    };
    let mut cmd = Command::new(program);
    cmd.args(arguments)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return (false, error.to_string()),
    };
    // Drain both pipes on their own threads: a child that prints more than
    // the pipe buffer would otherwise block on write and look like a timeout.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout_reader = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr_reader = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (
                    false,
                    format!("{program} timed out after {}s", timeout.as_secs()),
                );
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, error.to_string());
            }
        }
    };
    let mut bytes = stdout_reader.join().unwrap_or_default();
    bytes.extend(stderr_reader.join().unwrap_or_default());
    (ok, String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Time Machine local snapshots on the boot volume (read-only listing).
fn scan_snapshots() -> Vec<ToolItem> {
    let argv = [
        "/usr/bin/tmutil".to_string(),
        "listlocalsnapshots".into(),
        "/".into(),
    ];
    let result = run_bounded(&argv, Duration::from_secs(20));
    if result.status != CommandStatus::Success {
        return Vec::new();
    }
    parse_snapshots(&result.output)
        .into_iter()
        .map(|stamp| ToolItem {
            id: format!("snap:{stamp}"),
            label: stamp.clone(),
            detail: None,
            // tmutil reports no per-snapshot size; an honest None beats a guess.
            size_kb: None,
            requires_admin: true,
            target: ToolTarget::Snapshot { stamp },
        })
        .collect()
}

/// Homebrew cleanup preview: one item when a dry run reports space to free.
fn scan_brew(probes: &dyn LiveProbes) -> Option<ToolItem> {
    let brew = find_brew()?;
    // A concurrent brew (install/upgrade) owns the download cache; only a
    // provably idle brew may be previewed.
    if probes.any_process_running(&["brew"]) != TriState::Idle {
        return None;
    }
    let argv = [
        brew.to_string_lossy().into_owned(),
        "cleanup".into(),
        "--prune=all".into(),
        "-n".into(),
    ];
    let (ok, output) = run_bounded_env(&argv, BREW_ENV, Duration::from_secs(120));
    if !ok {
        return None;
    }
    let size_kb = parse_brew_free_kb(&output)?;
    if size_kb == 0 {
        return None;
    }
    Some(ToolItem {
        id: "brew:cleanup".into(),
        label: "Homebrew".into(),
        detail: Some(format!("{} items", count_brew_removals(&output))),
        size_kb: Some(size_kb),
        requires_admin: false,
        target: ToolTarget::BrewCleanup,
    })
}

/// Everything the tool section can offer. Cancellation between tools returns
/// an empty list: a cancelled scan must never feed a partial preview.
pub fn scan(probes: &dyn LiveProbes, cancel: &CancelFlag) -> Vec<ToolItem> {
    let mut items = Vec::new();
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    items.extend(scan_snapshots());
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    items.extend(scan_brew(probes));
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    items
}

/// Remove one tool item through its owning CLI. Identifiers are re-validated
/// here even though scan already did: the stored target is the only thing
/// between the webview and a command line.
pub fn execute(
    target: &ToolTarget,
    _home: &str,
    privileged: &dyn PrivilegedRunner,
) -> Result<(), String> {
    match target {
        ToolTarget::Snapshot { stamp } => {
            if !is_valid_snapshot_stamp(stamp) {
                return Err("invalid snapshot stamp".into());
            }
            privileged
                .delete_local_snapshot(stamp)
                .map_err(|e| e.to_string())
        }
        ToolTarget::BrewCleanup => {
            let brew = find_brew().ok_or_else(|| "brew is not available".to_string())?;
            let argv = [
                brew.to_string_lossy().into_owned(),
                "cleanup".into(),
                "--prune=all".into(),
            ];
            let (ok, output) = run_bounded_env(&argv, BREW_ENV, Duration::from_secs(600));
            if ok {
                Ok(())
            } else {
                Err(output)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_stamp_shape_is_strict() {
        assert!(is_valid_snapshot_stamp("2024-05-01-123456"));
        assert!(!is_valid_snapshot_stamp("2024-05-01-12345"));
        assert!(!is_valid_snapshot_stamp("2024-05-01-1234567"));
        assert!(!is_valid_snapshot_stamp("2024-05-01;123456"));
        assert!(!is_valid_snapshot_stamp("2024-05-01-12345a"));
        assert!(!is_valid_snapshot_stamp("2024/05/01-123456"));
        assert!(!is_valid_snapshot_stamp(""));
    }

    #[test]
    fn tmutil_lines_parse_and_garbage_is_dropped() {
        let out = "Snapshots for disk /:\n\
                   com.apple.TimeMachine.2024-05-01-123456.local\n\
                   com.apple.TimeMachine.2024-05-02-000000.local\n\
                   com.apple.TimeMachine.bogus.local\n\
                   com.apple.TimeMachine.2024-05-03-1234.local\n\
                   com.apple.TimeMachine.2024-05-04-123456\n\
                   \n";
        assert_eq!(
            parse_snapshots(out),
            vec![
                "2024-05-01-123456".to_string(),
                "2024-05-02-000000".to_string()
            ]
        );
        assert!(parse_snapshots("").is_empty());
    }

    #[test]
    fn brew_summary_line_parses_like_brew_prints_it() {
        let out = "Would remove: /opt/homebrew/Cellar/node/20.0.0 (3,000 files, 60MB)\n\
                   Would remove: /Users/x/Library/Caches/Homebrew/foo.bottle.tar.gz (12MB)\n\
                   This operation would free approximately 1.2GB of disk space.";
        assert_eq!(parse_brew_free_kb(out), Some(1_258_291));
        assert_eq!(count_brew_removals(out), 2);
        assert_eq!(
            parse_brew_free_kb("This operation would free approximately 383.4KB of disk space."),
            Some(383)
        );
        assert_eq!(
            parse_brew_free_kb("This operation would free approximately 512B of disk space."),
            Some(1)
        );
        assert_eq!(parse_brew_free_kb("Nothing to clean up."), None);
        assert_eq!(
            parse_brew_free_kb("This operation would free approximately 1.2PB of disk space."),
            None
        );
    }

    #[test]
    fn execute_rejects_malformed_identifiers_before_any_command() {
        let denied = mole_core::providers::DeniedPrivilegedRunner;
        let err = execute(
            &ToolTarget::Snapshot {
                stamp: "2024-05-01;123456".into(),
            },
            "/tmp",
            &denied,
        )
        .unwrap_err();
        assert_eq!(err, "invalid snapshot stamp");
        // A valid stamp reaches the runner, which (denied) refuses it.
        let err = execute(
            &ToolTarget::Snapshot {
                stamp: "2024-05-01-123456".into(),
            },
            "/tmp",
            &denied,
        )
        .unwrap_err();
        assert!(err.contains("not available"));
    }

    #[test]
    fn bounded_env_runner_passes_env_and_drains_output() {
        let (ok, out) = run_bounded_env(
            &[
                "/bin/sh".to_string(),
                "-c".into(),
                "echo $TIDY_TOOLS_TEST".into(),
            ],
            &[("TIDY_TOOLS_TEST", "hello")],
            Duration::from_secs(5),
        );
        assert!(ok);
        assert_eq!(out, "hello");
        let (ok, out) = run_bounded_env(
            &[
                "/usr/bin/head".to_string(),
                "-c".into(),
                "300000".into(),
                "/dev/zero".into(),
            ],
            &[],
            Duration::from_secs(5),
        );
        assert!(ok);
        assert_eq!(
            out.len(),
            300_000,
            "large output must be drained, not deadlock"
        );
        let (ok, out) = run_bounded_env(
            &["/bin/sleep".to_string(), "5".into()],
            &[],
            Duration::from_millis(100),
        );
        assert!(!ok);
        assert!(out.contains("timed out"));
    }
}

#[cfg(test)]
mod real_machine {
    /// Manual, read-only: cargo test -p mole-ops real_tools_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_tools_scan() {
        let probes = mole_core::probes::SystemProbes::new();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for item in super::scan(&probes, &cancel) {
            eprintln!(
                "{} | {} | {:?} | {:?} KB | admin={}",
                item.id, item.label, item.detail, item.size_kb, item.requires_admin
            );
        }
    }
}
