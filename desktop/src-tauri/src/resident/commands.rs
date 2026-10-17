// Resident IPC commands: thin translations from the panel / settings page to
// the resident shell and `mole_ops::resident`. Blocking work runs on
// spawn_blocking; the sampler is reached through its own thread.

use super::{lifecycle, panel, sampling, Resident};
use crate::error::IpcError;
use mole_ops::resident::memrelease::{self, ReleasePrefs, ReleaseReport};
use mole_ops::resident::prefs::ResidentPrefs;
use mole_ops::resident::{PanelTab, ResidentReading};
use std::sync::mpsc;
use tauri::{AppHandle, State};

/// Map a JoinError from spawn_blocking to the shared io code.
fn join_error(e: impl std::fmt::Display) -> IpcError {
    IpcError::new("io", e.to_string())
}

/// Current menu-bar prefs.
#[tauri::command]
pub fn resident_get_prefs(state: State<'_, Resident>) -> ResidentPrefs {
    state.prefs()
}

/// Normalize, apply to the status item, persist; returns the stored prefs.
#[tauri::command]
pub async fn resident_set_prefs(
    app: AppHandle,
    prefs: ResidentPrefs,
) -> Result<ResidentPrefs, IpcError> {
    tauri::async_runtime::spawn_blocking(move || super::apply_prefs(&app, prefs))
        .await
        .map_err(join_error)?
        .map_err(IpcError::from)
}

/// Latest cached sample (never samples).
#[tauri::command]
pub fn resident_get_reading(state: State<'_, Resident>) -> Option<ResidentReading> {
    state
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Force one sample now.
#[tauri::command]
pub async fn resident_refresh(state: State<'_, Resident>) -> Result<ResidentReading, IpcError> {
    let (tx, rx) = mpsc::channel();
    state.request(sampling::Request::Sample(tx));
    tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(join_error)?
        .map_err(|_| IpcError::new("unavailable", "sampler stopped"))
}

/// The panel reports its visible tab (drives which samples run).
#[tauri::command]
pub fn resident_select_tab(state: State<'_, Resident>, tab: PanelTab) {
    *state.tab.lock().unwrap_or_else(|e| e.into_inner()) = tab;
    state.wake();
}

/// The panel finished its first render.
#[tauri::command]
pub fn resident_panel_ready(app: AppHandle) {
    panel::ready(&app);
}

/// Show the panel from the main window ("System status" in settings),
/// even when the status item is hidden.
#[tauri::command]
pub fn resident_open_panel(app: AppHandle) -> Result<(), IpcError> {
    panel::open_from_main(&app).map_err(|e| IpcError::new("window", e.to_string()))
}

/// Hide the panel (Escape, footer actions).
#[tauri::command]
pub fn resident_hide_panel(app: AppHandle) {
    panel::hide(&app);
}

/// Hide the panel and show the main window at `destination`
/// ("main" | "settings" | "menubar_settings" | "clean").
#[tauri::command]
pub fn resident_open_main(app: AppHandle, destination: String) -> Result<(), IpcError> {
    let hash = lifecycle::destination_hash(&destination)
        .map_err(|()| IpcError::new("invalid", format!("unknown destination: {destination}")))?;
    lifecycle::show_main(&app, hash);
    Ok(())
}

/// Quit Tidy.
#[tauri::command]
pub fn resident_quit(app: AppHandle) {
    app.exit(0);
}

/// Purge purgeable memory now (blocking ~1 s).
#[tauri::command]
pub async fn resident_release_memory() -> Result<ReleaseReport, IpcError> {
    tauri::async_runtime::spawn_blocking(memrelease::release_now)
        .await
        .map_err(join_error)
}

/// Automatic release settings.
#[tauri::command]
pub fn resident_get_release_prefs() -> ReleasePrefs {
    memrelease::load_prefs(&crate::home())
}

/// Validate and persist automatic release settings.
#[tauri::command]
pub fn resident_set_release_prefs(prefs: ReleasePrefs) -> Result<ReleasePrefs, IpcError> {
    let home = crate::home();
    // save_prefs reports "invalid" for out-of-range values, "failed: …" for io.
    memrelease::save_prefs(&home, &prefs).map_err(|message| {
        let code = if message == "invalid" {
            "invalid"
        } else {
            "io"
        };
        IpcError::new(code, message)
    })?;
    Ok(memrelease::load_prefs(&home))
}

/// Ask an app to quit normally.
#[tauri::command]
pub async fn resident_quit_app(pid: u32) -> Result<(), IpcError> {
    tauri::async_runtime::spawn_blocking(move || mole_ops::resident::apps::quit_app(pid))
        .await
        .map_err(join_error)?
        .map_err(|code| {
            let code = match code.as_str() {
                "unavailable" => "unavailable",
                "unsupported" => "unsupported",
                _ => "failed",
            };
            IpcError::new(code, code)
        })
}

/// Zero the persisted cumulative network totals.
#[tauri::command]
pub async fn resident_reset_network(state: State<'_, Resident>) -> Result<(), IpcError> {
    let (tx, rx) = mpsc::channel();
    state.request(sampling::Request::ResetNetwork(tx));
    tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(join_error)?
        .map_err(|_| IpcError::new("unavailable", "sampler stopped"))?
        .map_err(IpcError::from)
}

/// Whether a newer Tidy version is known from the last real update check
/// (cached on disk, no network). Drives the panel footer's red dot.
#[tauri::command]
pub fn resident_update_available() -> bool {
    crate::update::update_known(&crate::home())
}

/// Network interfaces for the menu-bar settings' interface picker
/// (SystemConfiguration reads; blocking, so off the IPC thread).
#[tauri::command]
pub async fn resident_list_interfaces() -> Result<Vec<mole_ops::resident::InterfaceInfo>, IpcError>
{
    tauri::async_runtime::spawn_blocking(mole_ops::resident::list_interfaces)
        .await
        .map_err(join_error)
}
