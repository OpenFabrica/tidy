// Manual probe for the resident sampler: runs a Sampler at 1 Hz through each
// panel demand (overview, CPU, memory, disk, network tabs) and prints the
// JSON readings (histories trimmed to the last 5 slots) with per-call wall
// time and CPU cost. Run in release mode:
//
//   cargo run --release -p mole-ops --example resident_dump
//   cargo run --release -p mole-ops --example resident_dump -- --steady 60
//   cargo run --release -p mole-ops --example resident_dump -- --release-once
//   cargo run --release -p mole-ops --example resident_dump -- --quit-test
//
// `--steady N` measures N seconds of panel-closed sampling (CPU + memory
// shown, then every metric shown) and prints the average CPU share of one
// core. `--release-once` performs ONE real memory release and prints the
// report. `--quit-test` exercises quit_app with an invalid pid and our own
// (non-app) pid only.

use mole_ops::resident::{apps, memrelease, Demand, MetricId, PanelTab, Sampler};
use std::time::{Duration, Instant};

/// Process CPU time (user + system) so far.
fn cpu_time() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| {
        Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
    };
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// Keep only the newest 5 entries of every "*history" array and the first 5
/// app rows so the dump stays readable.
fn trim(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if let serde_json::Value::Array(a) = v {
                    if k.ends_with("history") && a.len() > 5 {
                        a.drain(..a.len() - 5);
                    } else if k.starts_with("apps_") && a.len() > 5 {
                        a.truncate(5);
                    }
                }
                trim(v);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(trim),
        _ => {}
    }
}

/// One timed sample; returns (wall, cpu) cost.
fn timed(s: &mut Sampler, d: &Demand) -> (mole_ops::resident::ResidentReading, Duration, Duration) {
    let (c0, t0) = (cpu_time(), Instant::now());
    let r = s.sample(d);
    (r, t0.elapsed(), cpu_time().saturating_sub(c0))
}

/// Steady-state CPU share of one core while sampling at 1 Hz for `secs`.
fn steady(home: &str, secs: u64, metrics: Vec<MetricId>, label: &str) {
    let mut s = Sampler::new(home);
    let d = Demand {
        panel_open: false,
        tab: PanelTab::Overview,
        metrics,
    };
    // Warm-up: first calls pay one-time costs (SMC key walk, static info).
    for _ in 0..3 {
        s.sample(&d);
        std::thread::sleep(Duration::from_secs(1));
    }
    let (c0, t0) = (cpu_time(), Instant::now());
    let mut worst = Duration::ZERO;
    for _ in 0..secs {
        let t = Instant::now();
        s.sample(&d);
        worst = worst.max(t.elapsed());
        std::thread::sleep(Duration::from_secs(1).saturating_sub(t.elapsed()));
    }
    let cpu = cpu_time().saturating_sub(c0);
    let wall = t0.elapsed();
    println!(
        "steady[{label}]: {secs} samples, cpu {:.1} ms total, {:.3} ms/sample, {:.3}% of one core, worst wall {:.2} ms",
        cpu.as_secs_f64() * 1000.0,
        cpu.as_secs_f64() * 1000.0 / secs as f64,
        cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
        worst.as_secs_f64() * 1000.0
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());

    if args.first().map(String::as_str) == Some("--release-once") {
        let t = Instant::now();
        let report = memrelease::release_now();
        println!(
            "release_now: {} in {:?}",
            serde_json::to_string(&report).unwrap(),
            t.elapsed()
        );
        return;
    }
    if args.first().map(String::as_str) == Some("--quit-test") {
        println!("quit_app(0) = {:?}", apps::quit_app(0));
        println!("quit_app(9999999) = {:?}", apps::quit_app(9_999_999));
        println!(
            "quit_app(own pid {}) = {:?}",
            std::process::id(),
            apps::quit_app(std::process::id())
        );
        println!("quit_app(1 launchd) = {:?}", apps::quit_app(1));
        return;
    }
    if args.first().map(String::as_str) == Some("--steady") {
        let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
        steady(
            &home,
            secs,
            vec![MetricId::Cpu, MetricId::Memory],
            "cpu+memory",
        );
        let all = mole_ops::resident::prefs::DEFAULT_ORDER.to_vec();
        steady(&home, secs, all, "all metrics");
        return;
    }

    // Use a throwaway home so the dump never touches the user's totals.
    let tmp = std::env::temp_dir().join(format!("resident-dump-{}", std::process::id()));
    let mut s = Sampler::new(tmp.to_str().unwrap());
    let phases = [
        ("overview", PanelTab::Overview),
        ("cpu", PanelTab::Cpu),
        ("memory", PanelTab::Memory),
        ("disk", PanelTab::Disk),
        ("network", PanelTab::Network),
    ];
    let all = mole_ops::resident::prefs::DEFAULT_ORDER.to_vec();
    for (label, tab) in phases {
        let d = Demand {
            panel_open: true,
            tab,
            metrics: all.clone(),
        };
        let mut last = None;
        for i in 0..3 {
            let (r, wall, cpu) = timed(&mut s, &d);
            println!(
                "[{label} #{i}] wall {:.2} ms, cpu {:.2} ms",
                wall.as_secs_f64() * 1000.0,
                cpu.as_secs_f64() * 1000.0
            );
            last = Some(r);
            std::thread::sleep(Duration::from_millis(1000));
        }
        let mut v = serde_json::to_value(last.unwrap()).unwrap();
        trim(&mut v);
        println!("{label}: {}", serde_json::to_string_pretty(&v).unwrap());
    }
}
