// Per-application CPU and memory for the panel's CPU / Memory tabs.
//
// Processes come from libproc: proc_pid_rusage gives CPU time and
// phys_footprint (Activity Monitor's "Memory" column) for our own user's
// processes. Other users' processes (WindowServer, root daemons) refuse
// that call; for those one bounded `ps -A` (setuid, so it can read them)
// supplies CPU time and resident size, and their memory is marked partial.
// Rows are grouped by the outermost .app bundle the executable lives in, so
// "Google Chrome" includes its helpers; loose processes stand alone.

use super::apps::RunningApp;
use super::ffi::run_bounded;
use super::AppUsage;
use crate::status::app_bundle_of;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Rows kept per list.
pub(crate) const MAX_ROWS: usize = 100;

/// One process observation.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProcRow {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
    /// CPU percent of one core (Activity Monitor style; may exceed 100).
    pub cpu: f32,
    /// phys_footprint; None when unreadable.
    pub footprint: Option<u64>,
    /// Resident size from ps when footprint is unreadable.
    pub rss_fallback: Option<u64>,
}

/// Counters from `ps` for processes libproc cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsCounter {
    pub cpu_ms: u64,
    pub rss: u64,
}

/// Parse ps TIME ("mm:ss.cc", "hh:mm:ss.cc", "d-hh:mm:ss.cc") into ms.
pub(crate) fn parse_ps_time(text: &str) -> Option<u64> {
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, text),
    };
    let (clock, frac) = rest.split_once('.').unwrap_or((rest, "0"));
    let mut secs = 0u64;
    for part in clock.split(':') {
        secs = secs
            .checked_mul(60)?
            .checked_add(part.parse::<u64>().ok()?)?;
    }
    let centis: u64 = format!("{frac:0<2}")[..2].parse().ok()?;
    days.checked_mul(86_400)?
        .checked_add(secs)?
        .checked_mul(1000)?
        .checked_add(centis * 10)
}

/// Parse `ps -A -o pid=,time=,rss=` (rss in KiB).
pub(crate) fn parse_ps(text: &str) -> HashMap<u32, PsCounter> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let cpu_ms = parse_ps_time(it.next()?)?;
            let rss = it.next()?.parse::<u64>().ok()?.saturating_mul(1024);
            Some((pid, PsCounter { cpu_ms, rss }))
        })
        .collect()
}

/// Every pid on the system.
fn list_pids() -> Vec<i32> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // Headroom for processes spawned between the two calls.
    let mut pids = vec![0i32; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as i32;
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if n <= 0 {
        return Vec::new();
    }
    pids.truncate(n as usize);
    pids.retain(|p| *p > 0);
    pids
}

/// rusage v2 for a pid; None for other users' processes.
fn rusage(pid: i32) -> Option<libc::rusage_info_v2> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr() as *mut libc::rusage_info_t,
        )
    };
    (rc == 0).then(|| unsafe { info.assume_init() })
}

/// Executable path (readable for every user's processes).
fn exe_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// Display name for a loose executable: its file name, except that
/// version-named binaries (~/.local/share/claude/versions/2.1.300) take the
/// nearest meaningful ancestor folder instead ("claude"), skipping version
/// folders and a literal "versions" directory on the way up.
fn display_name_of_exe(exe: &str) -> Option<String> {
    let is_version = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.');
    let mut parts = exe.rsplit('/').filter(|p| !p.is_empty());
    let file = parts.next()?;
    if !is_version(file) {
        return Some(file.to_string());
    }
    parts
        .find(|p| !is_version(p) && !p.eq_ignore_ascii_case("versions"))
        .map(str::to_string)
        .or_else(|| Some(file.to_string()))
}

/// Short process name (p_comm) when the path is unknown.
fn proc_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// Mach-tick → nanosecond ratio (1/1 on Intel, 125/3 on Apple Silicon);
/// rusage CPU times are reported in Mach ticks.
fn timebase() -> (u64, u64) {
    extern "C" {
        // mach_timebase_info_data_t is { numer: u32, denom: u32 }; declared
        // here because libc's binding is deprecated in favour of mach2.
        fn mach_timebase_info(info: *mut [u32; 2]) -> libc::c_int;
    }
    let mut tb = [0u32; 2];
    unsafe { mach_timebase_info(&mut tb) };
    match tb {
        [0, _] | [_, 0] => (1, 1),
        [numer, denom] => (u64::from(numer), u64::from(denom)),
    }
}

/// Owns the CPU-time baseline and path/name caches between samples.
pub(crate) struct ProcReader {
    timebase: (u64, u64),
    /// pid → (identity, cumulative CPU ns) from the previous sample.
    prev: HashMap<u32, (u64, u64)>,
    prev_at: Option<Instant>,
    /// (pid, identity) → executable path.
    paths: HashMap<(u32, u64), Option<String>>,
    /// bundle path → display name from Info.plist.
    bundle_names: HashMap<String, String>,
}

impl Default for ProcReader {
    /// Empty caches; the first CPU sample is a baseline.
    fn default() -> Self {
        Self {
            timebase: timebase(),
            prev: HashMap::new(),
            prev_at: None,
            paths: HashMap::new(),
            bundle_names: HashMap::new(),
        }
    }
}

impl ProcReader {
    /// Snapshot every process. With `want_cpu`, CPU percentages are deltas
    /// against the previous call; the returned flag is false when there was
    /// no recent baseline (percentages are all 0; retry a second later).
    pub(crate) fn sample(&mut self, want_cpu: bool) -> (Vec<ProcRow>, bool) {
        let now = Instant::now();
        let elapsed = self.prev_at.map(|t| now.saturating_duration_since(t));
        // A baseline older than 10 s would average over the idle gap.
        let baseline_ok = elapsed
            .is_some_and(|e| e >= Duration::from_millis(200) && e <= Duration::from_secs(10));
        let (numer, denom) = self.timebase;
        let mut rows = Vec::new();
        let mut cur = HashMap::new();
        let mut unreadable = Vec::new();
        for pid in list_pids() {
            let upid = pid as u32;
            let info = rusage(pid);
            let identity = info.map_or(0, |i| i.ri_proc_start_abstime);
            // Without a start-time identity a reused pid could inherit a stale
            // path, so unreadable processes are looked up every time.
            let exe = if identity == 0 {
                exe_path(pid)
            } else {
                self.paths
                    .entry((upid, identity))
                    .or_insert_with(|| exe_path(pid))
                    .clone()
            };
            let name = exe
                .as_deref()
                .and_then(display_name_of_exe)
                .or_else(|| proc_name(pid))
                .unwrap_or_else(|| format!("pid {pid}"));
            let mut row = ProcRow {
                pid: upid,
                name,
                exe,
                ..ProcRow::default()
            };
            match info {
                Some(i) => {
                    let ticks = i.ri_user_time.saturating_add(i.ri_system_time);
                    let ns = (u128::from(ticks) * u128::from(numer) / u128::from(denom)) as u64;
                    cur.insert(upid, (identity, ns));
                    row.footprint = Some(i.ri_phys_footprint);
                }
                None => unreadable.push(rows.len()),
            }
            rows.push(row);
        }
        // Other users' processes: CPU time + RSS via setuid ps. CPU time is in
        // centiseconds there, which is plenty for a 2 s window.
        if !unreadable.is_empty() {
            let ps = run_bounded(
                "/bin/ps",
                &["-A", "-o", "pid=,time=,rss="],
                Duration::from_millis(800),
            )
            .map(|out| parse_ps(&String::from_utf8_lossy(&out)))
            .unwrap_or_default();
            for idx in unreadable {
                let row = &mut rows[idx];
                if let Some(c) = ps.get(&row.pid) {
                    row.rss_fallback = Some(c.rss);
                    // Identity 1 marks "ps-sourced": never compared with a
                    // libproc baseline of the same pid.
                    cur.insert(row.pid, (1, c.cpu_ms.saturating_mul(1_000_000)));
                }
            }
        }
        if want_cpu && baseline_ok {
            let window = elapsed.unwrap_or_default().as_nanos().max(1) as f64;
            for row in &mut rows {
                let (Some((id, ns)), Some((pid_id, pns))) =
                    (cur.get(&row.pid), self.prev.get(&row.pid))
                else {
                    continue;
                };
                if id != pid_id {
                    continue; // pid reused by a new process
                }
                let delta = ns.saturating_sub(*pns) as f64;
                row.cpu = (delta / window * 100.0) as f32;
            }
        }
        let cpu_valid = want_cpu && baseline_ok;
        self.prev = cur;
        self.prev_at = Some(now);
        // Drop cached paths of exited processes.
        let live: std::collections::HashSet<u32> = rows.iter().map(|r| r.pid).collect();
        self.paths.retain(|(pid, _), _| live.contains(pid));
        (rows, cpu_valid)
    }

    /// Display name of a bundle from its Info.plist (cached), else the
    /// folder name without ".app".
    fn bundle_name(&mut self, bundle: &str) -> String {
        if let Some(n) = self.bundle_names.get(bundle) {
            return n.clone();
        }
        let from_plist = plist::Value::from_file(format!("{bundle}/Contents/Info.plist"))
            .ok()
            .and_then(|v| {
                let d = v.as_dictionary()?;
                ["CFBundleDisplayName", "CFBundleName"]
                    .iter()
                    .find_map(|k| d.get(k)?.as_string().map(str::to_string))
            })
            .filter(|n| !n.trim().is_empty());
        let name = from_plist.unwrap_or_else(|| {
            bundle
                .rsplit('/')
                .next()
                .unwrap_or(bundle)
                .trim_end_matches(".app")
                .to_string()
        });
        if self.bundle_names.len() > 2048 {
            self.bundle_names.clear();
        }
        self.bundle_names.insert(bundle.to_string(), name.clone());
        name
    }

    /// Group rows into applications (see module docs).
    pub(crate) fn group(
        &mut self,
        rows: &[ProcRow],
        running: &HashMap<u32, RunningApp>,
    ) -> Vec<AppUsage> {
        let mut groups: HashMap<String, Vec<&ProcRow>> = HashMap::new();
        for row in rows {
            let key = row
                .exe
                .as_deref()
                .and_then(app_bundle_of)
                .unwrap_or_else(|| format!("pid:{}", row.pid));
            groups.entry(key).or_default().push(row);
        }
        groups
            .into_iter()
            .map(|(key, members)| {
                let bundle = (!key.starts_with("pid:")).then(|| key.clone());
                let app_of = |r: &&ProcRow| running.get(&r.pid);
                // Representative: the regular app whose bundle is this group,
                // else any LaunchServices app, else the bundle's main binary,
                // else the lowest pid.
                let rep = members
                    .iter()
                    .filter(|r| app_of(r).is_some_and(|a| a.regular && a.bundle_path == bundle))
                    .min_by_key(|r| r.pid)
                    .or_else(|| {
                        members
                            .iter()
                            .filter(|r| app_of(r).is_some())
                            .min_by_key(|r| r.pid)
                    })
                    .or_else(|| {
                        let main = format!("{key}/Contents/MacOS/");
                        members
                            .iter()
                            .filter(|r| r.exe.as_deref().is_some_and(|e| e.starts_with(&main)))
                            .min_by_key(|r| r.pid)
                    })
                    .or_else(|| members.iter().min_by_key(|r| r.pid))
                    .copied()
                    .expect("group is never empty");
                let quittable = members.iter().any(|r| app_of(r).is_some_and(|a| a.regular));
                let name = match &bundle {
                    Some(b) => running
                        .get(&rep.pid)
                        .and_then(|a| a.name.clone())
                        .unwrap_or_else(|| self.bundle_name(b)),
                    None => rep.name.clone(),
                };
                let mut pids: Vec<u32> = members.iter().map(|r| r.pid).collect();
                pids.sort_unstable();
                let partial = members.iter().any(|r| r.footprint.is_none());
                let memory_bytes = members
                    .iter()
                    .map(|r| r.footprint.or(r.rss_fallback).unwrap_or(0))
                    .fold(0u64, u64::saturating_add);
                AppUsage {
                    key,
                    name,
                    bundle_path: bundle,
                    pid: rep.pid,
                    process_count: members.len() as u32,
                    pids,
                    executable: rep.exe.clone(),
                    cpu_percent: members.iter().map(|r| r.cpu).sum(),
                    memory_bytes,
                    partial,
                    quittable,
                }
            })
            .collect()
    }
}

/// Sort by CPU (desc, ties by name) and cap the list.
pub(crate) fn top_by_cpu(mut apps: Vec<AppUsage>) -> Vec<AppUsage> {
    apps.sort_by(|a, b| {
        b.cpu_percent
            .total_cmp(&a.cpu_percent)
            .then_with(|| a.name.cmp(&b.name))
    });
    apps.truncate(MAX_ROWS);
    apps
}

/// Sort by memory (desc, ties by name) and cap the list.
pub(crate) fn top_by_memory(mut apps: Vec<AppUsage>) -> Vec<AppUsage> {
    apps.sort_by(|a, b| {
        b.memory_bytes
            .cmp(&a.memory_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    apps.truncate(MAX_ROWS);
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_time_formats() {
        assert_eq!(parse_ps_time("0:00.12"), Some(120));
        assert_eq!(
            parse_ps_time("167:39.58"),
            Some((167 * 60 + 39) * 1000 + 580)
        );
        assert_eq!(parse_ps_time("1:02:03.40"), Some(3_723_400));
        assert_eq!(
            parse_ps_time("2-01:00:00.00"),
            Some((2 * 86_400 + 3600) * 1000)
        );
        assert_eq!(parse_ps_time("garbage"), None);
    }

    #[test]
    fn ps_output_parses_rows() {
        let m = parse_ps("    1 167:39.58  28768\n  77186 1022:11.58 181664\nbad line\n");
        assert_eq!(m.len(), 2);
        assert_eq!(m[&1].rss, 28768 * 1024);
        assert_eq!(m[&77186].cpu_ms, (1022 * 60 + 11) * 1000 + 580);
    }

    fn row(pid: u32, exe: &str, cpu: f32, mem: Option<u64>) -> ProcRow {
        ProcRow {
            pid,
            name: exe.rsplit('/').next().unwrap().to_string(),
            exe: Some(exe.to_string()),
            cpu,
            footprint: mem,
            rss_fallback: Some(7),
        }
    }

    #[test]
    fn version_named_binaries_take_their_tool_folder_name() {
        assert_eq!(
            display_name_of_exe("/Users/x/.local/share/claude/versions/2.1.300").as_deref(),
            Some("claude")
        );
        assert_eq!(
            display_name_of_exe("/usr/sbin/cfprefsd").as_deref(),
            Some("cfprefsd")
        );
        assert_eq!(display_name_of_exe("/1.2/3.4").as_deref(), Some("3.4"));
        assert_eq!(display_name_of_exe(""), None);
    }

    #[test]
    fn groups_helpers_under_their_app_and_keeps_loose_processes() {
        let chrome = "/Applications/Google Chrome.app";
        let rows = vec![
            row(10, &format!("{chrome}/Contents/MacOS/Google Chrome"), 5.0, Some(100)),
            row(
                11,
                &format!("{chrome}/Contents/Frameworks/X.framework/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)"),
                20.0,
                Some(300),
            ),
            row(20, "/usr/sbin/cfprefsd", 1.0, None),
        ];
        let running = HashMap::from([(
            10,
            RunningApp {
                name: Some("Google Chrome".into()),
                bundle_path: Some(chrome.into()),
                regular: true,
            },
        )]);
        let mut r = ProcReader::default();
        let apps = top_by_cpu(r.group(&rows, &running));
        assert_eq!(apps.len(), 2);
        let c = &apps[0];
        assert_eq!(c.key, chrome);
        assert_eq!(c.name, "Google Chrome");
        assert_eq!(c.pid, 10);
        assert_eq!(c.process_count, 2);
        assert_eq!(c.pids, vec![10, 11]);
        assert_eq!(c.cpu_percent, 25.0);
        assert_eq!(c.memory_bytes, 400);
        assert!(c.quittable && !c.partial);
        let loose = &apps[1];
        assert_eq!(loose.key, "pid:20");
        assert_eq!(loose.name, "cfprefsd");
        assert!(loose.partial && !loose.quittable);
        assert_eq!(loose.memory_bytes, 7, "ps RSS stands in when unreadable");
        assert_eq!(loose.bundle_path, None);
        let by_mem = top_by_memory(apps);
        assert_eq!(by_mem[0].key, chrome);
    }

    #[test]
    fn bundle_without_running_app_uses_folder_name_and_main_binary() {
        let rows = vec![
            row(
                31,
                "/Applications/Foo.app/Contents/Helpers/agent",
                0.0,
                Some(1),
            ),
            row(30, "/Applications/Foo.app/Contents/MacOS/Foo", 0.0, Some(1)),
        ];
        let mut r = ProcReader::default();
        let apps = r.group(&rows, &HashMap::new());
        assert_eq!(apps[0].name, "Foo");
        assert_eq!(apps[0].pid, 30);
        assert!(!apps[0].quittable);
    }

    #[test]
    fn caps_rows() {
        let apps: Vec<AppUsage> = (0..150)
            .map(|i| AppUsage {
                key: format!("pid:{i}"),
                cpu_percent: i as f32,
                ..AppUsage::default()
            })
            .collect();
        let top = top_by_cpu(apps);
        assert_eq!(top.len(), MAX_ROWS);
        assert_eq!(top[0].cpu_percent, 149.0);
    }

    /// Live: our own process is readable with a non-zero footprint.
    #[test]
    fn live_sample_reads_own_process() {
        let mut r = ProcReader::default();
        let (rows, cpu_ok) = r.sample(true);
        assert!(!cpu_ok, "first call is a baseline");
        let me = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .expect("own pid listed");
        assert!(me.footprint.unwrap_or(0) > 0);
    }
}
