// Main-window lifecycle and activation policy. While the status item is
// enabled Tidy is a menu-bar app: closing the main window hides it and drops
// the Dock icon (Accessory); opening it again restores the Dock icon
// (Regular). A login launch with `--background` starts hidden.

use super::Resident;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};

/// Launch argument the autostart LaunchAgent passes.
pub const BACKGROUND_ARG: &str = "--background";
/// Main window label.
pub const MAIN_LABEL: &str = "main";

/// Whether argv (program name first) asks for a background launch.
pub fn is_background_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter()
        .skip(1)
        .any(|a| a.as_ref() == BACKGROUND_ARG)
}

/// A background launch stays hidden only when the menu-bar item exists to
/// reach the app; otherwise the main window must appear.
pub fn start_hidden(background_launch: bool, resident_enabled: bool) -> bool {
    background_launch && resident_enabled
}

/// Startup: show the main window, or stay in the menu bar for a resident
/// login launch. The main window is configured `visible: false` so a hidden
/// start never flashes it.
pub fn startup(app: &AppHandle) {
    let enabled = app.state::<Resident>().enabled();
    if start_hidden(is_background_launch(std::env::args()), enabled) {
        set_dock(app, false);
    } else {
        show_main(app, None);
    }
}

/// Show and focus the main window, restoring the Dock icon, then optionally
/// navigate it (`mole-nav` with a route hash).
pub fn show_main(app: &AppHandle, hash: Option<&str>) {
    super::panel::hide(app);
    set_dock(app, true);
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(hash) = hash {
            let _ = app.emit_to(MAIN_LABEL, "mole-nav", hash.to_string());
        }
    }
}

/// Main window close button. With the status item enabled (or the legacy
/// keep-in-tray flag) the window only hides; the Dock icon goes away only
/// when the status item exists to bring the app back.
pub fn on_main_close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    let app = window.app_handle();
    let resident = app.state::<Resident>().enabled();
    if resident || crate::KEEP_IN_TRAY.load(Ordering::Relaxed) {
        api.prevent_close();
        let _ = window.hide();
        if resident {
            set_dock(app, false);
        }
    }
}

/// Dock icon on (Regular) / off (Accessory).
fn set_dock(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// Route hash for a `resident_open_main` destination; None = just show.
pub fn destination_hash(destination: &str) -> Result<Option<&'static str>, ()> {
    match destination {
        "main" => Ok(None),
        "settings" => Ok(Some("#/settings")),
        "menubar_settings" => Ok(Some("#/settings?section=menubar")),
        "clean" => Ok(Some("#/clean")),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_flag_is_read_after_the_program_name() {
        assert!(is_background_launch(["Tidy", "--background"]));
        assert!(!is_background_launch(["--background"]));
        assert!(!is_background_launch(["Tidy"]));
    }

    #[test]
    fn hidden_start_requires_the_status_item() {
        assert!(start_hidden(true, true));
        assert!(!start_hidden(true, false));
        assert!(!start_hidden(false, true));
    }

    #[test]
    fn destinations_map_to_route_hashes() {
        assert_eq!(destination_hash("main"), Ok(None));
        assert_eq!(
            destination_hash("menubar_settings"),
            Ok(Some("#/settings?section=menubar"))
        );
        assert_eq!(destination_hash("clean"), Ok(Some("#/clean")));
        assert!(destination_hash("nope").is_err());
    }
}
