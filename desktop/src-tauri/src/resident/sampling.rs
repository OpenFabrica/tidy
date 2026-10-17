// The resident sampling thread. It owns the (non-Sync) Sampler, ticks once a
// second while the status item is enabled or the panel is open, and blocks
// with no work at all otherwise. Each tick feeds the status item, caches the
// reading, and pushes it to the panel only while the panel is visible.

use super::columns::{self, ToneMemory};
use super::panel::PANEL_LABEL;
use super::status_item::{self, Content};
use super::Resident;
use mole_ops::resident::{Demand, ResidentReading, Sampler};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

/// Base tick: publish at most once a second.
const TICK: Duration = Duration::from_secs(1);

/// Event carrying a full `ResidentReading` to the panel.
pub const READING_EVENT: &str = "resident-reading";

/// Messages to the sampling thread.
pub enum Request {
    /// Demand changed (prefs, panel visibility, tab): re-evaluate the schedule.
    Wake,
    /// Prefs changed: rebuild the status item from the cached reading now.
    Repaint,
    /// Take one sample immediately and reply with it.
    Sample(mpsc::Sender<ResidentReading>),
    /// Zero the persisted network totals.
    ResetNetwork(mpsc::Sender<std::io::Result<()>>),
}

/// Spawn the sampling thread.
pub fn start(app: &AppHandle, rx: mpsc::Receiver<Request>) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("resident-sampler".into())
        .spawn(move || run(app, rx))
        .expect("spawn resident sampler");
}

/// Thread body: one loop multiplexing the 1 s tick and requests.
fn run(app: AppHandle, rx: mpsc::Receiver<Request>) {
    // Utility QoS keeps 1 Hz sampling on efficiency cores.
    mole_ops::scanutil::set_scan_thread_qos();
    let mut sampler = Sampler::new(&crate::home());
    let mut tones = ToneMemory::default();
    let mut last_sample: Option<Instant> = None;
    loop {
        let state = app.state::<Resident>();
        let active = state.enabled() || state.panel_open();
        let message = if active {
            let due = last_sample.map_or(Duration::ZERO, |at| TICK.saturating_sub(at.elapsed()));
            if due.is_zero() {
                Err(RecvTimeoutError::Timeout)
            } else {
                rx.recv_timeout(due)
            }
        } else {
            // Idle: no status item, no panel — sleep until something changes.
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match message {
            Ok(Request::Wake) => {}
            Ok(Request::Repaint) => repaint(&app, &mut tones),
            Ok(Request::Sample(reply)) => {
                let reading = tick(&app, &mut sampler, &mut tones);
                last_sample = Some(Instant::now());
                let _ = reply.send(reading);
            }
            Ok(Request::ResetNetwork(reply)) => {
                let _ = reply.send(sampler.reset_network_totals());
            }
            Err(RecvTimeoutError::Timeout) => {
                tick(&app, &mut sampler, &mut tones);
                last_sample = Some(Instant::now());
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// What consumers need right now.
fn demand(state: &Resident) -> Demand {
    let prefs = state.prefs();
    Demand {
        panel_open: state.panel_open(),
        tab: *state.tab.lock().unwrap_or_else(|e| e.into_inner()),
        metrics: if prefs.enabled {
            prefs.enabled_metrics()
        } else {
            Vec::new()
        },
    }
}

/// Take one sample and publish it everywhere it is needed.
fn tick(app: &AppHandle, sampler: &mut Sampler, tones: &mut ToneMemory) -> ResidentReading {
    let state = app.state::<Resident>();
    let mut reading = sampler.sample(&demand(&state));
    if reading.at_ms == 0 {
        reading.at_ms = now_ms();
    }
    *state.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(reading.clone());
    repaint(app, tones);
    if state.panel_open() {
        let _ = app.emit_to(PANEL_LABEL, READING_EVENT, &reading);
    }
    reading
}

/// Rebuild the status-item content from prefs + the cached reading.
fn repaint(app: &AppHandle, tones: &mut ToneMemory) {
    let state = app.state::<Resident>();
    let prefs = state.prefs();
    if !prefs.enabled {
        return;
    }
    let reading = state
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let columns = columns::build(&reading, &prefs, tones);
    let summary = columns::summary(&columns);
    status_item::publish(
        app,
        Content {
            icon: prefs.effective_icon(),
            compact: prefs.compact,
            columns,
            summary,
        },
    );
}

/// Wall-clock milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}
