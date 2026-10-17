// The resident sampler: one owner for every metric's cadence, rolling
// history and counter baseline. The shell calls `sample` about once a second
// from a single thread; each metric refreshes on its own interval (CPU 2 s,
// GPU 2 s, memory 3 s, network 1 s, disk I/O 2 s, disk capacity 30 s, CPU/GPU
// temperature 4 s, disk temperature 30 s) and the expensive extras (process
// lists, disk health, interface details, power, fans) run only while the
// `Demand` asks for them. Between refreshes a metric's latest fresh value
// fills the one-second history slots; see `history.rs` for the gap rules.

use super::history::{self, Advance, Series, Tracked};
use super::{
    apps, cpu, disk, diskhealth, gpu, memory, net, power, prefs, procs, AppUsage, CpuReading,
    Demand, DiskHealth, DiskReading, FanReading, GpuReading, MemoryReading, MetricId, NetDetails,
    NetworkReading, PanelTab, PowerReading, Pressure, ResidentReading,
};
use crate::smc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SEC: Duration = Duration::from_secs(1);
/// Disk health is slow to change and costs a plug-in round-trip.
const HEALTH_TTL: Duration = Duration::from_secs(300);
/// Interface details (addresses, Wi-Fi channel) cache.
const DETAILS_TTL: Duration = Duration::from_secs(10);
/// Re-resolve the automatic (default-route) interface this often.
const ROUTE_TTL: Duration = Duration::from_secs(10);
/// Check resident.json for a changed interface choice this often.
const PREFS_TTL: Duration = Duration::from_secs(5);
/// Persist cumulative network totals this often.
const TOTALS_SAVE_EVERY: Duration = Duration::from_secs(60);

/// Latest disk I/O rates plus the system totals they came from.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct IoRates {
    read_bps: u64,
    write_bps: u64,
    read_total: u64,
    write_total: u64,
}

/// What the current demand needs beyond the always-on metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Needs {
    cpu: bool,
    gpu: bool,
    memory: bool,
    disk: bool,
    network: bool,
    disk_io: bool,
    temps: bool,
    disk_temp: bool,
    power: bool,
    fans: bool,
    apps_cpu: bool,
    apps_memory: bool,
    health: bool,
    details: bool,
}

impl Needs {
    /// Translate a Demand into per-metric needs. "Needed" metrics count for
    /// staleness and keep showing their cached value; the gated ones are not
    /// sampled at all otherwise.
    fn from(demand: &Demand) -> Self {
        let has = |id: MetricId| demand.metrics.contains(&id);
        let open = demand.panel_open;
        let tab = |t: PanelTab| open && demand.tab == t;
        Needs {
            cpu: open || has(MetricId::Cpu),
            gpu: open || has(MetricId::Gpu),
            memory: open || has(MetricId::Memory),
            disk: open || has(MetricId::Disk),
            network: open || has(MetricId::Network),
            disk_io: tab(PanelTab::Disk),
            temps: open || has(MetricId::CpuTemperature) || has(MetricId::GpuTemperature),
            disk_temp: tab(PanelTab::Disk) || has(MetricId::DiskTemperature),
            power: open
                || [
                    MetricId::BatteryTemperature,
                    MetricId::ChargingPower,
                    MetricId::BatteryPower,
                    MetricId::SystemPower,
                ]
                .iter()
                .any(|m| has(*m)),
            fans: open || has(MetricId::FanSpeed),
            apps_cpu: tab(PanelTab::Cpu),
            apps_memory: tab(PanelTab::Memory),
            health: tab(PanelTab::Disk),
            details: tab(PanelTab::Network),
        }
    }
}

/// Owns every rolling history and the previous counters needed for rates.
/// Not Sync: one sampling thread owns it.
pub struct Sampler {
    home: String,
    last_call: Option<Instant>,
    needs: Needs,

    cpu_info: cpu::CpuInfo,
    prev_ticks: Option<cpu::Ticks>,
    cpu: Tracked<f32>,
    cpu_hist: Series<f32>,
    temps: Tracked<smc::Temperatures>,
    cpu_temp_hist: Series<f32>,
    gpu_temp_hist: Series<f32>,

    gpu: Tracked<gpu::GpuSample>,
    gpu_hist: Series<f32>,
    renderer_hist: Series<f32>,
    tiler_hist: Series<f32>,
    gpu_identity: (String, Option<u32>),

    memory: Tracked<(memory::MemorySample, Pressure)>,
    mem_hist: Series<f32>,

    capacity: Tracked<disk::Capacity>,
    io_reader: disk::IoReader,
    prev_io: Option<(Instant, disk::IoCounters)>,
    io: Tracked<IoRates>,
    io_hist: Series<(u64, u64)>,
    disk_temp: Tracked<f32>,
    health: Option<(Instant, DiskHealth)>,

    pref_iface: Option<String>,
    prefs_mtime: Option<SystemTime>,
    prefs_checked: Option<Instant>,
    auto_iface: Option<String>,
    route_checked: Option<Instant>,
    iface: Option<String>,
    prev_net: Option<(Instant, net::Counters)>,
    net: Tracked<(u64, u64)>,
    net_hist: Series<(u64, u64)>,
    totals: net::Totals,
    session: net::Totals,
    totals_dirty: bool,
    totals_saved: Instant,
    details: Option<(Instant, NetDetails)>,

    power: Tracked<PowerReading>,
    fans: Tracked<Vec<FanReading>>,

    procs: procs::ProcReader,
    apps_cpu: Tracked<Vec<AppUsage>>,
    apps_memory: Tracked<Vec<AppUsage>>,
}

/// Wall-clock now in Unix ms.
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Sampler {
    /// Create a sampler; `home` locates persisted network totals and the
    /// resident prefs (for the chosen network interface).
    pub fn new(home: &str) -> Self {
        Self {
            home: home.to_string(),
            last_call: None,
            needs: Needs::default(),
            cpu_info: cpu::info(),
            prev_ticks: None,
            cpu: Tracked::every(2 * SEC),
            cpu_hist: Series::default(),
            temps: Tracked::every(4 * SEC),
            cpu_temp_hist: Series::default(),
            gpu_temp_hist: Series::default(),
            gpu: Tracked::every(2 * SEC),
            gpu_hist: Series::default(),
            renderer_hist: Series::default(),
            tiler_hist: Series::default(),
            gpu_identity: (String::new(), None),
            memory: Tracked::every(3 * SEC),
            mem_hist: Series::default(),
            capacity: Tracked::every(30 * SEC),
            io_reader: disk::IoReader::default(),
            prev_io: None,
            io: Tracked::every(2 * SEC),
            io_hist: Series::default(),
            disk_temp: Tracked::every(30 * SEC),
            health: None,
            pref_iface: None,
            prefs_mtime: None,
            prefs_checked: None,
            auto_iface: None,
            route_checked: None,
            iface: None,
            prev_net: None,
            net: Tracked::every(SEC),
            net_hist: Series::default(),
            totals: net::Totals::load(home),
            session: net::Totals::default(),
            totals_dirty: false,
            totals_saved: Instant::now(),
            details: None,
            power: Tracked::every(2 * SEC),
            fans: Tracked::every(2 * SEC),
            procs: procs::ProcReader::default(),
            apps_cpu: Tracked::every(2 * SEC),
            apps_memory: Tracked::every(3 * SEC),
        }
    }

    /// Take one sample (call about once a second). Expensive parts follow
    /// `demand` and their own intervals; cheap metrics always refresh.
    pub fn sample(&mut self, demand: &Demand) -> ResidentReading {
        let now = Instant::now();
        let needs = Needs::from(demand);
        self.apply_demand_change(needs);
        let adv = history::advance(self.last_call, now);
        self.last_call = Some(now);

        self.sample_cpu(now, adv);
        self.sample_gpu(now, adv);
        self.sample_memory(now, adv);
        self.sample_disk(now, adv);
        self.sample_network(now, adv);
        self.sample_optional(now);

        self.build(now)
    }

    /// Zero the persisted cumulative network totals (all interfaces) and
    /// this session's totals, and persist immediately.
    pub fn reset_network_totals(&mut self) -> std::io::Result<()> {
        self.totals = net::Totals::default();
        self.session = net::Totals::default();
        self.totals_dirty = false;
        self.totals_saved = Instant::now();
        self.totals.save(&self.home)
    }

    /// When a gated metric becomes needed, sample it on this very call
    /// instead of waiting out its interval; drop lists no longer shown.
    fn apply_demand_change(&mut self, needs: Needs) {
        let old = self.needs;
        if needs.temps && !old.temps {
            self.temps.expire();
        }
        if needs.disk_temp && !old.disk_temp {
            self.disk_temp.expire();
        }
        if needs.power && !old.power {
            self.power.expire();
        }
        if needs.fans && !old.fans {
            self.fans.expire();
        }
        if needs.apps_cpu && !old.apps_cpu {
            self.apps_cpu = Tracked::every(2 * SEC);
        }
        if needs.apps_memory && !old.apps_memory {
            self.apps_memory = Tracked::every(3 * SEC);
        }
        if needs.details && !old.details {
            // Re-read when the tab opens; the cache only spans one visit.
            self.details = None;
        }
        self.needs = needs;
    }

    /// CPU usage from tick deltas (2 s) and CPU/GPU die temperatures (4 s).
    fn sample_cpu(&mut self, now: Instant, adv: Advance) {
        if self.cpu.due(now) {
            let ticks = cpu::read_ticks();
            let usage = match (self.prev_ticks, ticks) {
                (Some(p), Some(c)) => cpu::usage_between(p, c),
                _ => None,
            };
            if ticks.is_some() {
                self.prev_ticks = ticks;
            }
            self.cpu.record(now, usage);
            // The very first call only has a baseline: take the real sample
            // on the next call rather than two seconds later.
            if usage.is_none() && ticks.is_some() {
                self.cpu.expire();
            }
        }
        history::step(&mut self.cpu_hist, adv, self.cpu.fresh(now));

        if self.needs.temps && self.temps.due(now) {
            let t = smc::temperatures();
            let any = t.cpu_c.is_some() || t.gpu_c.is_some();
            self.temps.record(now, any.then_some(t));
        }
        let t = self.temps.fresh(now).unwrap_or_default();
        history::step(&mut self.cpu_temp_hist, adv, t.cpu_c);
        history::step(&mut self.gpu_temp_hist, adv, t.gpu_c);
    }

    /// GPU utilisation from IOAccelerator statistics (2 s).
    fn sample_gpu(&mut self, now: Instant, adv: Advance) {
        if self.gpu.due(now) {
            let s = gpu::read();
            if let Some(s) = &s {
                if !s.name.is_empty() {
                    self.gpu_identity = (s.name.clone(), s.core_count);
                }
            }
            self.gpu.record(now, s.filter(|s| s.usage.is_some()));
        }
        let g = self.gpu.fresh(now);
        history::step(&mut self.gpu_hist, adv, g.as_ref().and_then(|g| g.usage));
        history::step(
            &mut self.renderer_hist,
            adv,
            g.as_ref().and_then(|g| g.renderer),
        );
        history::step(&mut self.tiler_hist, adv, g.as_ref().and_then(|g| g.tiler));
    }

    /// Memory usage and pressure (3 s).
    fn sample_memory(&mut self, now: Instant, adv: Advance) {
        if self.memory.due(now) {
            let m = memory::read().map(|m| (m, memory::pressure()));
            self.memory.record(now, m);
        }
        history::step(
            &mut self.mem_hist,
            adv,
            self.memory.fresh(now).and_then(|(m, _)| m.usage_percent()),
        );
    }

    /// Boot-volume capacity (30 s) and system-wide block I/O (2 s).
    fn sample_disk(&mut self, now: Instant, adv: Advance) {
        if self.capacity.due(now) {
            self.capacity.record(now, disk::capacity());
        }
        if self.io.due(now) {
            let cur = self.io_reader.read();
            let rates = match (&self.prev_io, &cur) {
                (Some((at, prev)), Some(cur)) => {
                    let elapsed = now.saturating_duration_since(*at);
                    let (dr, dw) = disk::io_delta(prev, cur);
                    let (rt, wt) = cur.totals();
                    match (history::rate(0, dr, elapsed), history::rate(0, dw, elapsed)) {
                        (Some(read_bps), Some(write_bps)) => Some(IoRates {
                            read_bps,
                            write_bps,
                            read_total: rt,
                            write_total: wt,
                        }),
                        _ => None,
                    }
                }
                _ => None,
            };
            let baseline_only = rates.is_none() && cur.is_some() && self.prev_io.is_none();
            if let Some(cur) = cur {
                self.prev_io = Some((now, cur));
            }
            self.io.record(now, rates);
            if baseline_only {
                self.io.expire();
            }
        }
        history::step(
            &mut self.io_hist,
            adv,
            self.io.fresh(now).map(|r| (r.read_bps, r.write_bps)),
        );
    }

    /// Re-read the chosen interface from resident.json when the file changed
    /// (cheap stat every 5 s), so the shell never has to forward it.
    fn refresh_pref_iface(&mut self, now: Instant) {
        if self
            .prefs_checked
            .is_some_and(|t| now.saturating_duration_since(t) < PREFS_TTL)
        {
            return;
        }
        self.prefs_checked = Some(now);
        let mtime = std::fs::metadata(prefs::prefs_file(&self.home))
            .and_then(|m| m.modified())
            .ok();
        if mtime != self.prefs_mtime {
            self.prefs_mtime = mtime;
            self.pref_iface = prefs::load(&self.home)
                .network_interface
                .filter(|s| !s.is_empty());
        }
    }

    /// Interface to report: the user's choice when it exists, else the
    /// default-route interface (re-resolved every 10 s or when it vanishes).
    fn resolve_iface(&mut self, now: Instant, counters: &net::Counters) -> Option<String> {
        self.refresh_pref_iface(now);
        if let Some(p) = &self.pref_iface {
            if counters.contains_key(p) {
                return Some(p.clone());
            }
        }
        let gone = self
            .auto_iface
            .as_ref()
            .is_some_and(|i| !counters.contains_key(i));
        let expired = self
            .route_checked
            .is_none_or(|t| now.saturating_duration_since(t) >= ROUTE_TTL);
        if gone || expired {
            self.route_checked = Some(now);
            self.auto_iface = net::default_interface();
        }
        self.auto_iface.clone()
    }

    /// Interface rates (1 s), cumulative/session totals and periodic save.
    fn sample_network(&mut self, now: Instant, adv: Advance) {
        if self.net.due(now) {
            let cur = net::read_counters();
            let mut rates = None;
            if let Some(cur) = &cur {
                let iface = self.resolve_iface(now, cur);
                if iface != self.iface {
                    // A different link's past minute would be misleading.
                    self.net_hist = Series::default();
                    self.iface = iface.clone();
                }
                if let Some((at, prev)) = &self.prev_net {
                    self.totals.accumulate(prev, cur);
                    self.session.accumulate(prev, cur);
                    self.totals_dirty = true;
                    if let Some(name) = &iface {
                        let elapsed = now.saturating_duration_since(*at);
                        if let (Some((prx, ptx)), Some((rx, tx))) = (prev.get(name), cur.get(name))
                        {
                            rates = history::rate(*prx, *rx, elapsed)
                                .zip(history::rate(*ptx, *tx, elapsed));
                        }
                    }
                }
            }
            let baseline_only = rates.is_none() && cur.is_some() && self.prev_net.is_none();
            if let Some(cur) = cur {
                self.prev_net = Some((now, cur));
            }
            self.net.record(now, rates);
            if baseline_only {
                self.net.expire();
            }
        }
        history::step(&mut self.net_hist, adv, self.net.fresh(now));

        if self.totals_dirty
            && now.saturating_duration_since(self.totals_saved) >= TOTALS_SAVE_EVERY
        {
            self.totals_saved = now;
            // A failed save keeps the dirty flag and retries next minute.
            if self.totals.save(&self.home).is_ok() {
                self.totals_dirty = false;
            }
        }
    }

    /// Demand-gated extras: disk temperature/health, interface details,
    /// power, fans and the per-app lists.
    fn sample_optional(&mut self, now: Instant) {
        let n = self.needs;
        if n.disk_temp && self.disk_temp.due(now) {
            self.disk_temp.record(now, diskhealth::temperature());
        }
        if n.health
            && self
                .health
                .as_ref()
                .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= HEALTH_TTL)
        {
            self.health = Some((now, diskhealth::read()));
        }
        if n.details {
            if let Some(name) = self.iface.clone() {
                let fresh = self.details.as_ref().is_some_and(|(at, d)| {
                    d.interface == name && now.saturating_duration_since(*at) < DETAILS_TTL
                });
                if !fresh {
                    self.details = Some((now, net::details(&name)));
                }
            }
        }
        if n.power && self.power.due(now) {
            self.power.record(now, Some(power::read()));
        }
        if n.fans && self.fans.due(now) {
            let fans = smc::fans()
                .into_iter()
                .enumerate()
                .map(|(i, f)| FanReading {
                    id: i as u32,
                    rpm: f.actual_rpm,
                })
                .collect();
            self.fans.record(now, Some(fans));
        }
        if n.apps_cpu && self.apps_cpu.due(now) {
            let (rows, cpu_ok) = self.procs.sample(true);
            if cpu_ok {
                let grouped = self.procs.group(&rows, &apps::running_apps());
                self.apps_cpu.record(now, Some(procs::top_by_cpu(grouped)));
            } else {
                // Baseline only: real percentages need a second snapshot.
                self.apps_cpu.record(now, None);
                self.apps_cpu.expire();
            }
        }
        if n.apps_memory && self.apps_memory.due(now) {
            let (rows, _) = self.procs.sample(false);
            let grouped = self.procs.group(&rows, &apps::running_apps());
            self.apps_memory
                .record(now, Some(procs::top_by_memory(grouped)));
        }
    }

    /// Assemble the published reading from the tracked values.
    fn build(&self, now: Instant) -> ResidentReading {
        let n = self.needs;
        let info = &self.cpu_info;
        let (cpu_avg, cpu_peak) = self.cpu_hist.avg_peak();
        let temps = self.temps.shown(now, n.temps).unwrap_or_default();
        let cpu = CpuReading {
            usage: self.cpu.shown(now, n.cpu),
            model: info.model.clone(),
            core_count: info.core_count,
            e_cores: info.e_cores,
            p_cores: info.p_cores,
            core_levels: info.levels.clone(),
            avg_1m: cpu_avg,
            peak_1m: cpu_peak,
            temp_c: temps.cpu_c,
            history: self.cpu_hist.to_vec(),
            temp_history: self.cpu_temp_hist.to_vec(),
        };

        let g = self.gpu.shown(now, n.gpu);
        let (gpu_avg, gpu_peak) = self.gpu_hist.avg_peak();
        let gpu = GpuReading {
            usage: g.as_ref().and_then(|g| g.usage),
            name: self.gpu_identity.0.clone(),
            core_count: self.gpu_identity.1,
            renderer: g.as_ref().and_then(|g| g.renderer),
            tiler: g.as_ref().and_then(|g| g.tiler),
            memory_used_bytes: g.as_ref().and_then(|g| g.memory_used_bytes),
            avg_1m: gpu_avg,
            peak_1m: gpu_peak,
            temp_c: temps.gpu_c,
            history: self.gpu_hist.to_vec(),
            temp_history: self.gpu_temp_hist.to_vec(),
            renderer_history: self.renderer_hist.to_vec(),
            tiler_history: self.tiler_hist.to_vec(),
        };

        let m = self.memory.shown(now, n.memory);
        let memory = MemoryReading {
            total_bytes: m.map_or(0, |(m, _)| m.total),
            used_bytes: m.map_or(0, |(m, _)| m.used),
            free_bytes: m.map_or(0, |(m, _)| m.free),
            swap_used_bytes: m.map_or(0, |(m, _)| m.swap_used),
            usage_percent: m.and_then(|(m, _)| m.usage_percent()),
            pressure: m.map_or(Pressure::Unknown, |(_, p)| p),
            history: self.mem_hist.to_vec(),
        };

        let cap = self.capacity.shown(now, n.disk);
        let io = self.io.shown(now, n.disk_io);
        let disk = DiskReading {
            volume_name: cap.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            total_bytes: cap.as_ref().map_or(0, |c| c.total),
            available_bytes: cap.as_ref().map_or(0, |c| c.available),
            used_percent: cap.as_ref().and_then(|c| c.used_percent()),
            read_bps: io.map(|r| r.read_bps),
            write_bps: io.map(|r| r.write_bps),
            read_total_bytes: io.map(|r| r.read_total),
            write_total_bytes: io.map(|r| r.write_total),
            io_history: self.io_hist.to_vec(),
            temp_c: self.disk_temp.shown(now, n.disk_temp),
            health: if n.health {
                self.health.as_ref().map(|(_, h)| h.clone())
            } else {
                None
            },
        };

        let rates = self.net.shown(now, n.network);
        let (total, session) = match &self.iface {
            Some(i) => (self.totals.get(i), self.session.get(i)),
            None => Default::default(),
        };
        let network = NetworkReading {
            interface: self.iface.clone(),
            rx_bps: rates.map(|r| r.0),
            tx_bps: rates.map(|r| r.1),
            history: self.net_hist.to_vec(),
            total_rx_bytes: total.rx,
            total_tx_bytes: total.tx,
            session_rx_bytes: session.rx,
            session_tx_bytes: session.tx,
            details: if n.details {
                self.details.as_ref().map(|(_, d)| d.clone())
            } else {
                None
            },
        };

        let stale = (n.cpu && self.cpu.is_stale(now))
            || (n.gpu && self.gpu.is_stale(now))
            || (n.memory && self.memory.is_stale(now))
            || (n.disk && self.capacity.is_stale(now))
            || (n.disk_io && self.io.is_stale(now))
            || (n.network && self.net.is_stale(now))
            || (n.temps && self.temps.is_stale(now))
            || (n.disk_temp && self.disk_temp.is_stale(now))
            || (n.power && self.power.is_stale(now))
            || (n.fans && self.fans.is_stale(now))
            || (n.apps_cpu && self.apps_cpu.is_stale(now))
            || (n.apps_memory && self.apps_memory.is_stale(now));

        ResidentReading {
            at_ms: unix_ms(),
            cpu,
            gpu,
            memory,
            disk,
            network,
            power: self.power.shown(now, n.power).unwrap_or_default(),
            fans: self.fans.shown(now, n.fans).unwrap_or_default(),
            apps_cpu: if n.apps_cpu {
                self.apps_cpu.last().unwrap_or_default()
            } else {
                Vec::new()
            },
            apps_memory: if n.apps_memory {
                self.apps_memory.last().unwrap_or_default()
            } else {
                Vec::new()
            },
            stale,
        }
    }
}

impl Drop for Sampler {
    /// Best-effort flush of unsaved network totals on shutdown.
    fn drop(&mut self) {
        if self.totals_dirty {
            let _ = self.totals.save(&self.home);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shell moves the sampler onto its own thread.
    #[test]
    fn sampler_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Sampler>();
    }

    #[test]
    fn needs_follow_demand() {
        let closed = Needs::from(&Demand {
            panel_open: false,
            tab: PanelTab::Cpu,
            metrics: vec![MetricId::Cpu, MetricId::Memory],
        });
        assert!(closed.cpu && closed.memory);
        assert!(!closed.temps && !closed.power && !closed.fans);
        assert!(!closed.apps_cpu, "lists only while the panel is open");

        let cpu_tab = Needs::from(&Demand {
            panel_open: true,
            tab: PanelTab::Cpu,
            metrics: vec![],
        });
        assert!(cpu_tab.apps_cpu && !cpu_tab.apps_memory && cpu_tab.temps);
        assert!(!cpu_tab.health && !cpu_tab.details && !cpu_tab.disk_io);

        let disk_tab = Needs::from(&Demand {
            panel_open: true,
            tab: PanelTab::Disk,
            metrics: vec![],
        });
        assert!(disk_tab.health && disk_tab.disk_io && disk_tab.disk_temp);

        let power = Needs::from(&Demand {
            panel_open: false,
            tab: PanelTab::Overview,
            metrics: vec![MetricId::SystemPower],
        });
        assert!(power.power && !power.fans);
    }

    /// Live end-to-end: two samples a second apart yield CPU, memory and
    /// network values with full-length histories; process lists stay empty
    /// while the panel is closed.
    #[test]
    fn live_samples_fill_core_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Sampler::new(dir.path().to_str().unwrap());
        let demand = Demand {
            panel_open: false,
            tab: PanelTab::Overview,
            metrics: vec![MetricId::Cpu, MetricId::Memory],
        };
        let _ = s.sample(&demand);
        std::thread::sleep(Duration::from_millis(1050));
        let r = s.sample(&demand);
        assert!(r.cpu.usage.is_some());
        assert_eq!(r.cpu.history.len(), crate::resident::HISTORY_LEN);
        assert!(r.memory.usage_percent.is_some());
        assert!(r.memory.total_bytes > 0);
        assert!(r.disk.total_bytes > 0);
        assert!(r.apps_cpu.is_empty() && r.apps_memory.is_empty());
        assert!(!r.stale);
        s.reset_network_totals().unwrap();
        assert!(net::Totals::file(dir.path().to_str().unwrap()).exists());
    }
}
