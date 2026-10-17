// Scan utilities shared by every feature: cancellable physical size
// measurement (du -skP semantics: no symlink following, allocated blocks) and
// candidate construction with identity binding at discovery time.

use mole_core::identity;
use mole_core::plan::{Candidate, Scope, SizeKb};
use std::ffi::{CString, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Cancellation flag shared between an IPC command and its scan workers.
pub type CancelFlag = Arc<AtomicBool>;

extern "C" {
    /// From <pthread/qos.h>; not exposed by the libc crate.
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// Share of the machine's logical cores a scan may occupy: 8/10. A per-request
/// cap alone is insufficient because independent Analyze, Clean, and Apps
/// requests can otherwise multiply each other's CPU load, so this budget is
/// process-wide. Spending 80% is safe because every walker runs at Utility QoS
/// (efficiency cores, see `set_scan_thread_qos`); the >700% CPU incident was
/// default-QoS threads, not thread count.
const SCAN_CPU_NUMERATOR: usize = 8;
const SCAN_CPU_DENOMINATOR: usize = 10;

/// Process-wide walk budget for this machine: 8/10 of the logical cores, at
/// least two so a dual-core box still overlaps IO with work. Computed once —
/// core count does not change while the process runs.
pub fn global_scan_workers() -> usize {
    static WORKERS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WORKERS.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        (cores * SCAN_CPU_NUMERATOR / SCAN_CPU_DENOMINATOR).max(2)
    })
}

/// Process-wide counting semaphore for recursive metadata walks. It uses only
/// the standard library so the low-level scan boundary stays dependency-free.
struct ScanLimiter {
    active: Mutex<usize>,
    ready: Condvar,
    limit: usize,
}

impl ScanLimiter {
    const fn new(limit: usize) -> Self {
        Self {
            active: Mutex::new(0),
            ready: Condvar::new(),
            limit,
        }
    }

    /// Wait for one process-wide worker slot. Timed waits keep cancellation
    /// responsive even when every slot is occupied by another feature.
    fn acquire<'a>(&'a self, cancel: &AtomicBool) -> Result<ScanPermit<'a>, Cancelled> {
        let mut active = self.active.lock().unwrap();
        while *active >= self.limit {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            let (next, _) = self
                .ready
                .wait_timeout(active, Duration::from_millis(25))
                .unwrap();
            active = next;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        *active += 1;
        Ok(ScanPermit { limiter: self })
    }
}

/// RAII worker slot: every success, cancellation, and panic-unwind path
/// releases capacity for the next queued scan.
struct ScanPermit<'a> {
    limiter: &'a ScanLimiter,
}

impl Drop for ScanPermit<'_> {
    fn drop(&mut self) {
        let mut active = self.limiter.active.lock().unwrap();
        *active -= 1;
        self.limiter.ready.notify_one();
    }
}

/// Lazily sized so the budget follows the machine it runs on.
static SCAN_LIMITER: std::sync::LazyLock<ScanLimiter> =
    std::sync::LazyLock::new(|| ScanLimiter::new(global_scan_workers()));

/// Move the calling scan worker to the Utility QoS band so heavy tree walks
/// land on efficiency cores and never compete with the UI thread. A full-speed
/// default-QoS scan showed up as >700% CPU in the process table. Best-effort:
/// on failure the thread just keeps default scheduling.
pub fn set_scan_thread_qos() {
    const QOS_CLASS_UTILITY: u32 = 0x11; // <sys/qos.h>
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
    }
}

/// Bounded worker count for parallel size measurement. Each request stays at
/// or below the process-wide budget, while `SCAN_LIMITER` also coordinates
/// independent requests and product features.
pub fn scan_workers(jobs: usize) -> usize {
    jobs.clamp(1, global_scan_workers())
}

/// Error returned when a scan was cancelled mid-flight; partial output must
/// never feed a deletion plan.
#[derive(Debug)]
pub struct Cancelled;

/// Process-wide live counters behind the scan progress screen: entries
/// checked, allocated bytes walked, and a recent directory. They only ever
/// grow — several scans run in parallel and none of them owns a reset — so a
/// reader takes a baseline when its scan starts and shows the difference.
static METER_ITEMS: AtomicU64 = AtomicU64::new(0);
static METER_BYTES: AtomicU64 = AtomicU64::new(0);
static METER_DIRS: AtomicU64 = AtomicU64::new(0);
static METER_PATH: Mutex<String> = Mutex::new(String::new());

/// Publish the current path once every this many directories: a string copy
/// per directory would cost more than the walk itself on small trees.
const METER_PATH_EVERY: u64 = 16;

/// Point-in-time copy of the scan meter.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScanMeter {
    pub items: u64,
    pub bytes: u64,
    pub path: String,
}

/// Read the scan meter (cheap; safe to poll from the UI several times a second).
pub fn scan_meter() -> ScanMeter {
    ScanMeter {
        items: METER_ITEMS.load(Ordering::Relaxed),
        bytes: METER_BYTES.load(Ordering::Relaxed),
        path: METER_PATH.lock().map(|p| p.clone()).unwrap_or_default(),
    }
}

/// Add one batch of walked entries to the meter.
fn meter_add(items: u64, blocks: u64) {
    METER_ITEMS.fetch_add(items, Ordering::Relaxed);
    METER_BYTES.fetch_add(blocks * 512, Ordering::Relaxed);
}

/// Record the directory being walked, sampled and without ever blocking a
/// walker: a contended lock just skips this sample.
fn meter_visit(dir: &Path) {
    if !METER_DIRS
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(METER_PATH_EVERY)
    {
        return;
    }
    if let Ok(mut current) = METER_PATH.try_lock() {
        current.clear();
        current.push_str(&dir.to_string_lossy());
    }
}

/// Cancellable recursive walk behind the size measurement. The node itself
/// is stat'ed once; its children go through the bulk lister when possible.
fn walk_tree(
    path: &Path,
    blocks: &mut u64,
    items: &mut u64,
    cancel: &AtomicBool,
) -> Result<bool, Cancelled> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        // Unreadable entries make the total a lower bound, not a failure.
        Err(_) => return Ok(false),
    };
    *blocks += meta.blocks();
    *items += 1;
    meter_add(1, meta.blocks());
    if meta.is_dir() {
        walk_children(path, blocks, items, cancel)?;
    }
    Ok(true)
}

/// Sum a directory's children. Fast path: one `getattrlistbulk` call per
/// directory returns name, type and allocated size for every entry (what
/// Finder and `du` use) instead of one `stat` syscall per file. Falls back to
/// `read_dir` + `stat` when the volume does not support bulk attributes.
fn walk_children(
    dir: &Path,
    blocks: &mut u64,
    items: &mut u64,
    cancel: &AtomicBool,
) -> Result<(), Cancelled> {
    meter_visit(dir);
    let Some(entries) = bulk::list(dir) else {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                walk_tree(&entry.path(), blocks, items, cancel)?;
            }
        }
        return Ok(());
    };
    // Publish this directory's own entries in one batch (subdirectories
    // publish theirs when they are walked), not one atomic per entry.
    let mut dir_blocks = 0u64;
    for entry in &entries {
        dir_blocks += entry.alloc_bytes / 512;
    }
    meter_add(entries.len() as u64, dir_blocks);
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        *items += 1;
        // allocsize is bytes; keep the 512-byte block unit of the stat path.
        *blocks += entry.alloc_bytes / 512;
        if entry.is_dir {
            walk_children(&dir.join(&entry.name), blocks, items, cancel)?;
        }
    }
    Ok(())
}

/// Minimal `getattrlistbulk(2)` binding: list one directory with the three
/// attributes the size walk needs. Symlinks are reported as themselves
/// (FSOPT_NOFOLLOW), never followed.
mod bulk {
    use super::*;

    /// One directory entry from the bulk lister.
    pub struct Entry {
        pub name: OsString,
        pub is_dir: bool,
        pub alloc_bytes: u64,
    }

    // <sys/attr.h> / <sys/vnode.h> values the libc crate does not export.
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const VDIR: u32 = 2;
    const BUF_LEN: usize = 256 * 1024;

    /// Read a little-endian scalar from an unaligned 4-byte-packed buffer.
    fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
        buf.get(at..at + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// See [`read_u32`]; off_t fields are 8 bytes but only 4-byte aligned.
    fn read_u64(buf: &[u8], at: usize) -> Option<u64> {
        buf.get(at..at + 8)
            .map(|b| u64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// List `dir`. None means "use the stat fallback" (open failed, volume
    /// lacks bulk support, or the reply was malformed).
    pub fn list(dir: &Path) -> Option<Vec<Entry>> {
        let c_path = CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: plain open(2) on a NUL-terminated path; the fd is closed
        // below on every exit path.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return None;
        }
        let result = list_fd(fd);
        // SAFETY: fd came from the open above and is not used afterwards.
        unsafe { libc::close(fd) };
        result
    }

    /// Drain the bulk iterator on an open directory fd.
    fn list_fd(fd: libc::c_int) -> Option<Vec<Entry>> {
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_RETURNED_ATTRS
                | ATTR_CMN_ERROR
                | libc::ATTR_CMN_NAME
                | libc::ATTR_CMN_OBJTYPE,
            volattr: 0,
            dirattr: libc::ATTR_DIR_ALLOCSIZE,
            fileattr: libc::ATTR_FILE_ALLOCSIZE,
            forkattr: 0,
        };
        let mut buf = vec![0u8; BUF_LEN];
        let mut out = Vec::new();
        loop {
            // SAFETY: attrs and buf outlive the call; buf length is passed
            // exactly; the kernel writes at most BUF_LEN bytes.
            let n = unsafe {
                libc::getattrlistbulk(
                    fd,
                    &mut attrs as *mut libc::attrlist as *mut libc::c_void,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                    libc::FSOPT_NOFOLLOW as u64,
                )
            };
            if n < 0 {
                return None;
            }
            if n == 0 {
                return Some(out);
            }
            let mut pos = 0usize;
            for _ in 0..n {
                let len = read_u32(&buf, pos)? as usize;
                if len < 4 + 20 || pos + len > buf.len() {
                    return None;
                }
                let entry = &buf[pos..pos + len];
                // attribute_set_t: five u32 groups, we need common/dir/file.
                let ret_common = read_u32(entry, 4)?;
                let ret_dir = read_u32(entry, 12)?;
                let ret_file = read_u32(entry, 16)?;
                let mut cur = 24;
                let mut error = 0;
                if ret_common & ATTR_CMN_ERROR != 0 {
                    error = read_u32(entry, cur)?;
                    cur += 4;
                }
                let mut name = OsString::new();
                if ret_common & libc::ATTR_CMN_NAME != 0 {
                    // attrreference_t: offset is relative to the reference
                    // itself; length includes the trailing NUL.
                    let off = read_u32(entry, cur)? as i32 as isize;
                    let nlen = read_u32(entry, cur + 4)? as usize;
                    let start = (cur as isize + off) as usize;
                    let bytes = entry.get(start..start + nlen)?;
                    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
                    name = OsString::from_vec(bytes.to_vec());
                    cur += 8;
                }
                let mut is_dir = false;
                if ret_common & libc::ATTR_CMN_OBJTYPE != 0 {
                    is_dir = read_u32(entry, cur)? == VDIR;
                    cur += 4;
                }
                let mut alloc_bytes = 0;
                if ret_dir & libc::ATTR_DIR_ALLOCSIZE != 0 {
                    alloc_bytes = read_u64(entry, cur)?;
                    cur += 8;
                }
                if ret_file & libc::ATTR_FILE_ALLOCSIZE != 0 {
                    alloc_bytes = read_u64(entry, cur)?;
                }
                pos += len;
                if error != 0 || name.is_empty() {
                    continue;
                }
                out.push(Entry {
                    name,
                    is_dir,
                    alloc_bytes,
                });
            }
        }
    }
}

/// Physical size of a tree in KB plus item count, without following symlinks.
/// Returns Err(Cancelled) as soon as the flag flips — the caller discards.
/// Background users run this on utility-QoS workers (`set_scan_thread_qos`)
/// instead of duty-cycle throttling, so warmups are fast yet stay off the
/// performance cores.
pub fn dir_size_kb(path: &Path, cancel: &AtomicBool) -> Result<(SizeKb, u64), Cancelled> {
    let _permit = SCAN_LIMITER.acquire(cancel)?;
    let mut blocks = 0u64;
    let mut items = 0u64;
    walk_tree(path, &mut blocks, &mut items, cancel)?;
    Ok((SizeKb::Known(blocks * 512 / 1024), items))
}

/// Directories deeper than this below the measured root are folded into
/// their nearest kept ancestor instead of getting their own entry. Keeps the
/// result map small on trees like node_modules while every directory a user
/// can realistically drill into (≤ 6 clicks) stays a cache hit.
const TREE_KEEP_DEPTH: usize = 6;

/// One kept directory during a parallel walk. `name` + `parent` replace a
/// full PathBuf per node so a million-directory tree stays in tens of MB.
struct TreeNode {
    name: std::ffi::OsString,
    parent: Option<usize>,
    blocks: u64,
    items: u64,
}

/// Subtree totals (KB, item count) for `root` and every kept directory
/// below it, from ONE walk. This is what makes Analyze drill-down cheap:
/// the top-level scan already visited every file, so child listings are
/// map lookups instead of fresh walks.
pub struct TreeSizes {
    pub dirs: std::collections::HashMap<std::path::PathBuf, (u64, u64)>,
}

/// Minimum gap between two progress reports. The walk visits directories far
/// faster than a UI can paint, so reporting every one would flood the IPC
/// channel; a tenth of a second still reads as continuous motion.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Parallel work-stealing directory walk: every directory is one job, so a
/// single huge child (~/Library) no longer pins one thread while the others
/// idle. Semantics match `dir_size_kb` exactly (symlinks counted as
/// themselves, unreadable dirs skipped, the root counts as one item).
/// `progress` receives (directories visited so far, the directory just
/// listed), throttled to `PROGRESS_INTERVAL`, so a long walk can prove it is
/// still alive.
pub fn measure_tree(
    root: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(usize, &Path) + Sync,
) -> Result<TreeSizes, Cancelled> {
    let root_meta = fs::symlink_metadata(root).map_err(|_| Cancelled)?;
    if !root_meta.is_dir() {
        let mut dirs = std::collections::HashMap::new();
        dirs.insert(root.to_path_buf(), (root_meta.blocks() * 512 / 1024, 1));
        return Ok(TreeSizes { dirs });
    }
    let nodes = Mutex::new(vec![TreeNode {
        name: root.as_os_str().to_os_string(),
        parent: None,
        blocks: root_meta.blocks(),
        items: 1,
    }]);
    // Jobs: (path, node that receives this dir's totals, depth).
    let queue: Mutex<Vec<(std::path::PathBuf, usize, usize)>> =
        Mutex::new(vec![(root.to_path_buf(), 0, 0)]);
    let in_flight = std::sync::atomic::AtomicUsize::new(1);
    // Progress bookkeeping: a visit counter plus the millisecond stamp of the
    // last report. The stamp is claimed with compare_exchange so exactly one
    // worker reports per interval, without holding a lock across the callback.
    let visited = std::sync::atomic::AtomicUsize::new(0);
    let started = std::time::Instant::now();
    let last_report = AtomicU64::new(0);

    let workers = scan_workers(usize::MAX);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                set_scan_thread_qos();
                let Ok(_permit) = SCAN_LIMITER.acquire(cancel) else {
                    return;
                };
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let job = queue.lock().unwrap().pop();
                    let Some((path, agg, depth)) = job else {
                        if in_flight.load(Ordering::Acquire) == 0 {
                            return;
                        }
                        std::thread::sleep(Duration::from_micros(200));
                        continue;
                    };
                    let (mut blocks, mut items, subdirs) = list_one_level(&path);
                    // Subdirectories: kept ones become nodes (owning their
                    // own entry: 1 item + the dir's own blocks, like
                    // dir_size_kb counts a root); deeper ones fold into `agg`.
                    let mut new_jobs = Vec::with_capacity(subdirs.len());
                    let mut nodes = nodes.lock().unwrap();
                    for (name, dir_blocks) in subdirs {
                        if depth < TREE_KEEP_DEPTH {
                            let idx = nodes.len();
                            nodes.push(TreeNode {
                                name: name.clone(),
                                parent: Some(agg),
                                blocks: dir_blocks,
                                items: 1,
                            });
                            new_jobs.push((path.join(name), idx, depth + 1));
                        } else {
                            blocks += dir_blocks;
                            items += 1;
                            new_jobs.push((path.join(name), agg, depth + 1));
                        }
                    }
                    nodes[agg].blocks += blocks;
                    nodes[agg].items += items;
                    drop(nodes);
                    in_flight.fetch_add(new_jobs.len(), Ordering::AcqRel);
                    queue.lock().unwrap().extend(new_jobs);
                    in_flight.fetch_sub(1, Ordering::AcqRel);

                    let done = visited.fetch_add(1, Ordering::Relaxed) + 1;
                    let now = started.elapsed().as_millis() as u64;
                    let last = last_report.load(Ordering::Relaxed);
                    if now.saturating_sub(last) >= PROGRESS_INTERVAL.as_millis() as u64
                        && last_report
                            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok()
                    {
                        progress(done, &path);
                    }
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }

    // Roll totals up: children were always pushed after their parent, so a
    // reverse pass sees every child before its parent.
    let mut nodes = nodes.into_inner().unwrap();
    for i in (1..nodes.len()).rev() {
        let (blocks, items, parent) = (nodes[i].blocks, nodes[i].items, nodes[i].parent);
        if let Some(p) = parent {
            nodes[p].blocks += blocks;
            nodes[p].items += items;
        }
    }
    // Rebuild full paths top-down (parents have lower indices).
    let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(nodes.len());
    let mut dirs = std::collections::HashMap::with_capacity(nodes.len());
    for node in &nodes {
        let path = match node.parent {
            None => std::path::PathBuf::from(&node.name),
            Some(p) => paths[p].join(&node.name),
        };
        dirs.insert(path.clone(), (node.blocks * 512 / 1024, node.items));
        paths.push(path);
    }
    Ok(TreeSizes { dirs })
}

/// List one directory: (blocks and count of NON-directory entries, then the
/// subdirectories as (name, the dir entry's own blocks)). Bulk path first,
/// stat fallback second — same rules as `walk_children`.
fn list_one_level(dir: &Path) -> (u64, u64, Vec<(std::ffi::OsString, u64)>) {
    meter_visit(dir);
    let (blocks, items, subdirs) = list_one_level_raw(dir);
    // Feed the scan meter with every entry of this level: files plus the
    // subdirectory entries themselves (their contents publish when listed).
    let dir_blocks: u64 = subdirs.iter().map(|(_, b)| *b).sum();
    meter_add(items + subdirs.len() as u64, blocks + dir_blocks);
    (blocks, items, subdirs)
}

/// The listing behind `list_one_level`, without meter bookkeeping.
fn list_one_level_raw(dir: &Path) -> (u64, u64, Vec<(std::ffi::OsString, u64)>) {
    let mut blocks = 0;
    let mut items = 0;
    let mut subdirs = Vec::new();
    if let Some(entries) = bulk::list(dir) {
        for entry in entries {
            if entry.is_dir {
                subdirs.push((entry.name, entry.alloc_bytes / 512));
            } else {
                items += 1;
                blocks += entry.alloc_bytes / 512;
            }
        }
        return (blocks, items, subdirs);
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                subdirs.push((entry.file_name(), meta.blocks()));
            } else {
                items += 1;
                blocks += meta.blocks();
            }
        }
    }
    (blocks, items, subdirs)
}

/// Build a candidate for an existing path: snapshot identity and measure size.
/// None when the path is gone (raced away between listing and binding).
pub fn make_candidate(
    path: &Path,
    section: &str,
    scope: Scope,
    cancel: &AtomicBool,
) -> Option<Candidate> {
    let identity = identity::snapshot(path.to_str()?)?;
    let (size_kb, item_count) = dir_size_kb(path, cancel).ok()?;
    Some(Candidate {
        id: next_id(),
        path: path.to_string_lossy().into_owned(),
        size_kb,
        item_count,
        scope,
        section: section.to_string(),
        identity: Some(identity),
    })
}

/// Session-unique candidate id: monotonic counter, no RNG dependency.
pub fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("c{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a small fixture tree and return its root.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("f1"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("a/f2"), vec![0u8; 1024]).unwrap();
        fs::write(root.join("a/b/f3"), b"x").unwrap();
        (dir, root)
    }

    /// The pool tracks the machine: never more than 8/10 of the cores, never
    /// more jobs than exist, and never zero. Walks stay at Utility QoS, which
    /// is what keeps the budget off the performance cores.
    #[test]
    fn scan_worker_pool_follows_cpu_count() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let budget = global_scan_workers();
        assert!(budget >= 2, "at least two workers everywhere");
        assert!(
            budget <= cores.max(2),
            "budget {budget} must not exceed {cores} cores"
        );
        assert_eq!(budget, (cores * 8 / 10).max(2));
        assert_eq!(scan_workers(0), 1);
        assert_eq!(scan_workers(1), 1);
        assert_eq!(scan_workers(usize::MAX), budget);
    }

    /// Multiple product scans share one process-wide budget. Per-call worker
    /// caps alone allowed three Analyze requests to run six hot walks after a
    /// WebView reload, which reproduced as roughly 500% CPU on macOS.
    #[test]
    fn scan_limiter_caps_concurrent_walks_across_callers() {
        let limiter = Arc::new(ScanLimiter::new(2));
        let cancel = Arc::new(AtomicBool::new(false));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for _ in 0..6 {
                let limiter = limiter.clone();
                let cancel = cancel.clone();
                let active = active.clone();
                let peak = peak.clone();
                scope.spawn(move || {
                    let _permit = limiter.acquire(&cancel).unwrap();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn directory_walk_waiting_for_global_budget_is_cancellable() {
        let (_guard, root) = fixture();
        let occupied_cancel = AtomicBool::new(false);
        // Exhaust the whole process budget so the walk below must wait.
        let _held: Vec<_> = (0..global_scan_workers())
            .map(|_| SCAN_LIMITER.acquire(&occupied_cancel).unwrap())
            .collect();
        let waiting_cancel = Arc::new(AtomicBool::new(false));

        let waiter = {
            let waiting_cancel = waiting_cancel.clone();
            std::thread::spawn(move || dir_size_kb(&root, &waiting_cancel).is_err())
        };
        std::thread::sleep(Duration::from_millis(10));
        waiting_cancel.store(true, Ordering::Relaxed);

        assert!(waiter.join().unwrap());
    }

    /// Reference walk: read_dir + stat for every entry (the pre-bulk path).
    fn stat_walk(path: &Path, blocks: &mut u64, items: &mut u64) {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return;
        };
        *blocks += meta.blocks();
        *items += 1;
        if meta.is_dir() {
            if let Ok(rd) = fs::read_dir(path) {
                for e in rd.flatten() {
                    stat_walk(&e.path(), blocks, items);
                }
            }
        }
    }

    /// The bulk lister must agree with the stat walk and with `du -sk`
    /// (symlinks counted as themselves, never followed; hidden names kept).
    #[test]
    fn bulk_walk_matches_stat_walk_and_du() {
        let (_guard, root) = fixture();
        fs::write(root.join(".hidden"), vec![1u8; 8192]).unwrap();
        std::os::unix::fs::symlink("/", root.join("a/rootlink")).unwrap();
        fs::write(root.join("a/b/ünïcode name"), vec![2u8; 300]).unwrap();

        let cancel = AtomicBool::new(false);
        let (SizeKb::Known(kb), items) = dir_size_kb(&root, &cancel).unwrap() else {
            panic!("size must be known");
        };
        let (mut blocks, mut ref_items) = (0, 0);
        stat_walk(&root, &mut blocks, &mut ref_items);
        assert_eq!(items, ref_items);
        assert_eq!(kb, blocks * 512 / 1024);

        let du = std::process::Command::new("du")
            .args(["-skP"])
            .arg(&root)
            .output()
            .unwrap();
        let du_kb: u64 = String::from_utf8_lossy(&du.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(kb, du_kb);
        assert!(
            bulk::list(&root).is_some(),
            "fixture volume supports bulk listing"
        );
    }

    /// The parallel tree walk must agree with the serial walk for the root
    /// and for every kept subdirectory.
    #[test]
    fn measure_tree_matches_dir_size_kb() {
        let (_guard, root) = fixture();
        std::os::unix::fs::symlink("/", root.join("a/rootlink")).unwrap();
        // A chain deeper than TREE_KEEP_DEPTH: folded, still counted.
        let mut deep = root.join("d");
        for _ in 0..TREE_KEEP_DEPTH + 3 {
            deep = deep.join("x");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("leaf"), vec![7u8; 5000]).unwrap();

        let cancel = AtomicBool::new(false);
        let tree = measure_tree(&root, &cancel, |_, _| {}).unwrap();
        for dir in [
            root.clone(),
            root.join("a"),
            root.join("a/b"),
            root.join("d"),
        ] {
            let (SizeKb::Known(kb), items) = dir_size_kb(&dir, &cancel).unwrap() else {
                panic!()
            };
            assert_eq!(tree.dirs.get(&dir), Some(&(kb, items)), "{}", dir.display());
        }
        assert!(!tree.dirs.contains_key(&deep), "folded dirs get no entry");
        assert!(measure_tree(&root, &AtomicBool::new(true), |_, _| {}).is_err());
    }

    /// Every measured entry and byte must reach the process-wide meter (other
    /// tests walk concurrently, so the check is "grew by at least").
    #[test]
    fn walk_feeds_scan_meter() {
        let (_guard, root) = fixture();
        let before = scan_meter();
        let cancel = AtomicBool::new(false);
        let (SizeKb::Known(kb), items) = dir_size_kb(&root, &cancel).unwrap() else {
            panic!("size must be known");
        };
        let after = scan_meter();
        assert!(after.items - before.items >= items);
        assert!(after.bytes - before.bytes >= kb * 1024);

        // Analyze's parallel tree walk feeds the same meter (every entry
        // below the root; the root itself is not listed by anyone).
        let before = scan_meter();
        measure_tree(&root, &cancel, |_, _| {}).unwrap();
        let after = scan_meter();
        assert!(after.items - before.items >= items - 1);
    }

    #[test]
    fn walk_honors_cancellation() {
        let (_guard, root) = fixture();
        let cancel = AtomicBool::new(true);
        assert!(dir_size_kb(&root, &cancel).is_err());
    }
}

/// Measure many paths in parallel with a bounded worker pool; order preserved.
/// Cancelled scans yield None entries so partial results cannot masquerade as
/// complete candidates.
pub fn parallel_candidates(
    paths: &[std::path::PathBuf],
    section: &str,
    scope: Scope,
    cancel: &CancelFlag,
) -> Vec<Candidate> {
    let workers = scan_workers(paths.len());
    let queue = std::sync::Mutex::new(paths.iter().cloned().enumerate().collect::<Vec<_>>());
    let results = std::sync::Mutex::new(vec![None; paths.len()]);

    std::thread::scope(|scope_| {
        for _ in 0..workers {
            scope_.spawn(|| {
                set_scan_thread_qos();
                loop {
                    let job = queue.lock().unwrap().pop();
                    let Some((idx, path)) = job else { break };
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let candidate = make_candidate(&path, section, scope, cancel);
                    results.lock().unwrap()[idx] = candidate;
                }
            });
        }
    });

    results
        .into_inner()
        .unwrap()
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod bench {
    use super::*;
    /// Manual timing: cargo test -p mole-ops bench_node_modules -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_node_modules() {
        let path = std::env::var("BENCH_DIR").unwrap_or_else(|_| "../../ui/node_modules".into());
        let p = Path::new(&path);
        let cancel = AtomicBool::new(false);
        fn stat_walk(path: &Path, blocks: &mut u64, items: &mut u64) {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return;
            };
            *blocks += meta.blocks();
            *items += 1;
            if meta.is_dir() {
                if let Ok(rd) = fs::read_dir(path) {
                    for e in rd.flatten() {
                        stat_walk(&e.path(), blocks, items);
                    }
                }
            }
        }
        // Warm the cache once with each, then time both twice.
        for round in 0..2 {
            let t = std::time::Instant::now();
            let (SizeKb::Known(kb), n) = dir_size_kb(p, &cancel).unwrap() else {
                panic!()
            };
            eprintln!("round {round} bulk: {kb} KB, {n} items, {:?}", t.elapsed());
            let t = std::time::Instant::now();
            let (mut b, mut i) = (0, 0);
            stat_walk(p, &mut b, &mut i);
            eprintln!(
                "round {round} stat: {} KB, {i} items, {:?}",
                b * 512 / 1024,
                t.elapsed()
            );
        }
    }
}
