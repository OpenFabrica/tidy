// Resident (menu-bar) shell: the live status item, its tray panel window,
// the 1 s sampling thread, automatic memory release and the app's activation
// policy. Data and policy live in `mole_ops::resident`; this module only
// wires them to AppKit, Tauri windows and IPC.

pub mod columns;
pub mod commands;
pub mod lifecycle;
pub mod memauto;
pub mod menu;
pub mod panel;
pub mod sampling;
pub mod status_item;

use mole_ops::resident::prefs::{self, ResidentPrefs};
use mole_ops::resident::{PanelTab, ResidentReading};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// Status-item id (kept from the old static tray so existing code paths agree).
pub const TRAY_ID: &str = "mole-tray";

/// Shared resident state, managed by Tauri.
pub struct Resident {
    /// Current menu-bar prefs (normalized); persisted on every change.
    pub prefs: Mutex<ResidentPrefs>,
    /// Latest published sample, for `resident_get_reading` and panel seeding.
    pub latest: Mutex<Option<ResidentReading>>,
    /// Panel window is on screen (drives Demand and event emission).
    pub panel_open: AtomicBool,
    /// Panel page finished its first render.
    pub panel_ready: AtomicBool,
    /// Tab the panel reports as visible.
    pub tab: Mutex<PanelTab>,
    /// Serializes panel creation between prewarm and an early click.
    pub panel_creation: Mutex<()>,
    /// Requests to the sampling thread (which owns the non-Sync Sampler).
    sampler: Mutex<mpsc::Sender<sampling::Request>>,
}

impl Resident {
    /// Snapshot of the current prefs.
    pub fn prefs(&self) -> ResidentPrefs {
        self.prefs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether the status item is enabled ("Show in menu bar").
    pub fn enabled(&self) -> bool {
        self.prefs.lock().unwrap_or_else(|e| e.into_inner()).enabled
    }

    /// Whether the panel is currently shown.
    pub fn panel_open(&self) -> bool {
        self.panel_open.load(Ordering::Relaxed)
    }

    /// Send a request to the sampling thread; a dead thread is ignored.
    pub fn request(&self, request: sampling::Request) {
        let _ = self
            .sampler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(request);
    }

    /// Re-evaluate demand now (prefs / panel visibility / tab changed).
    pub fn wake(&self) {
        self.request(sampling::Request::Wake);
    }
}

/// Resolve the invoking user's home (same rule as the rest of the shell).
fn home() -> String {
    crate::home()
}

/// Create the status item, the resident state and its worker threads.
/// Called from `setup` on the main thread.
pub fn install(app: &tauri::App) -> tauri::Result<()> {
    let prefs = prefs::load(&home());
    let (tx, rx) = mpsc::channel();
    app.manage(Resident {
        prefs: Mutex::new(prefs.clone()),
        latest: Mutex::new(None),
        panel_open: AtomicBool::new(false),
        panel_ready: AtomicBool::new(false),
        tab: Mutex::new(PanelTab::default()),
        panel_creation: Mutex::new(()),
        sampler: Mutex::new(tx),
    });

    // No `.menu(..)`: a permanently attached NSStatusItem.menu makes macOS 27
    // swallow primary clicks (tray-icon #365). `menu::show` attaches the menu
    // only for the duration of a right-click tracking session.
    let icon = tauri::image::Image::from_bytes(crate::tray_anim::IDLE_PNG)?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(false)
        .tooltip("Tidy")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            // Act on press, not release: a release must never reopen a panel
            // the same press just closed.
            if let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Down,
                ..
            } = event
            {
                match button {
                    MouseButton::Left => {
                        let app = tray.app_handle().clone();
                        // Leave the native callback before touching windows.
                        tauri::async_runtime::spawn(async move { panel::toggle(&app) });
                    }
                    MouseButton::Right => {
                        panel::hide(tray.app_handle());
                        menu::show(tray);
                    }
                    MouseButton::Middle => {}
                }
            }
        })
        .build(app)?;
    tray.set_visible(prefs.enabled)?;
    app.on_menu_event(|app, event| menu::handle(app, event.id().as_ref()));

    let handle = app.handle().clone();
    sampling::start(&handle, rx);
    memauto::start(&handle);
    if prefs.enabled {
        panel::prewarm(&handle);
    }
    Ok(())
}

/// Apply new prefs: status item first (visibility + immediate repaint from
/// the cached reading), then persist. Returns the stored value.
pub fn apply_prefs(app: &AppHandle, next: ResidentPrefs) -> Result<ResidentPrefs, std::io::Error> {
    let next = next.normalized();
    let state = app.state::<Resident>();
    let was_enabled = {
        let mut current = state.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let was = current.enabled;
        *current = next.clone();
        was
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if was_enabled != next.enabled {
            let _ = tray.set_visible(next.enabled);
            // A re-shown item is a fresh NSStatusItem with the stock icon.
            status_item::refresh(app);
        }
    }
    if next.enabled {
        state.request(sampling::Request::Repaint);
        if !was_enabled {
            panel::prewarm(app);
        }
    } else if was_enabled {
        // Turning the status item off leaves no menu-bar entry: hide the
        // panel and bring the main window (and Dock icon) back.
        panel::hide(app);
        lifecycle::show_main(app, None);
    }
    state.wake();
    prefs::save(&home(), &next)?;
    Ok(next)
}
