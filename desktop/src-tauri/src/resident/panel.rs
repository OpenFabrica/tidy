// The tray panel window: one prewarmed hidden WebView (`panel.html`) shown
// centered under the status item. It hides on blur (unless that press is on
// the status item itself, whose own toggle closes it), on a status-item
// re-click or right-click, on Escape (page calls `resident_hide_panel`) and
// when the main window opens.

use super::sampling::READING_EVENT;
use super::{Resident, TRAY_ID};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

/// Panel window label (also used by its capability file).
pub const PANEL_LABEL: &str = "tray-panel";
/// Logical panel size.
const WIDTH: f64 = 390.0;
const HEIGHT: f64 = 610.0;
/// Logical gap between the menu bar and the panel.
const GAP: f64 = 6.0;
/// Event telling the panel it was shown (true) or hidden (false).
pub const VISIBILITY_EVENT: &str = "resident-panel-visibility";

/// Create the hidden panel off the setup path so the first click is instant.
pub fn prewarm(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = ensure_created(&app) {
            eprintln!("resident panel prewarm failed: {error}");
        }
    });
}

/// Left click on the status item: show, or hide when already open.
pub fn toggle(app: &AppHandle) {
    if app.state::<Resident>().panel_open() {
        hide(app);
    } else if let Err(error) = open(app) {
        eprintln!("resident panel open failed: {error}");
    }
}

/// Focus-loss events before this instant are ignored: opening from the main
/// window hands focus over in several steps, and the panel must not take a
/// transient blur from that handover as "user clicked elsewhere".
static BLUR_GRACE_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
/// Length of that grace period.
const BLUR_GRACE: Duration = Duration::from_millis(600);

/// Show the panel under the status item and seed it with the cached reading.
pub fn open(app: &AppHandle) -> tauri::Result<()> {
    if !app.state::<Resident>().enabled() {
        return Ok(());
    }
    open_unchecked(app)
}

/// Main-window "System status" button: open even with the status item
/// off (then the panel sits under the menu bar near the right edge) and
/// survive the blur caused by the main window giving up focus.
pub fn open_from_main(app: &AppHandle) -> tauri::Result<()> {
    *BLUR_GRACE_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + BLUR_GRACE);
    open_unchecked(app)
}

/// Shared open path without the enabled check.
fn open_unchecked(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<Resident>();
    state.panel_open.store(true, Ordering::Relaxed);
    if let Err(error) = ensure_created(app).and_then(|()| show(app)) {
        // A failed show must not keep open intent (it would keep detailed
        // sampling on and turn the next click into a hide).
        hide(app);
        return Err(error);
    }
    state.wake();
    Ok(())
}

/// Hide the panel (no-op when it is not open).
pub fn hide(app: &AppHandle) {
    let state = app.state::<Resident>();
    let was_open = state.panel_open.swap(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(PANEL_LABEL) {
        let _ = window.hide();
        if was_open {
            let _ = app.emit_to(PANEL_LABEL, VISIBILITY_EVENT, false);
        }
    }
    if was_open {
        state.wake();
    }
}

/// The page finished its first render: seed it if it is already visible.
pub fn ready(app: &AppHandle) {
    let state = app.state::<Resident>();
    state.panel_ready.store(true, Ordering::Relaxed);
    if state.panel_open() {
        seed(app);
    }
}

/// Push the cached reading so an opening panel never shows empty cards.
fn seed(app: &AppHandle) {
    let latest = app
        .state::<Resident>()
        .latest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(reading) = latest {
        let _ = app.emit_to(PANEL_LABEL, READING_EVENT, &reading);
    }
}

/// Build the hidden panel window once.
fn ensure_created(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<Resident>();
    let _creation = state
        .panel_creation
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if app.get_webview_window(PANEL_LABEL).is_some() {
        return Ok(());
    }
    let builder = WebviewWindowBuilder::new(app, PANEL_LABEL, WebviewUrl::App("panel.html".into()))
        .title("Tidy")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .shadow(true);
    // A titled window keeps the system rounded frame and shadow; the overlay
    // title bar lets the page fill it. Accept-first-mouse delivers the first
    // click to the page even while another app is active.
    #[cfg(target_os = "macos")]
    let builder = builder
        .accept_first_mouse(true)
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let window = builder.build()?;
    #[cfg(target_os = "macos")]
    window.with_webview(|webview| {
        use objc2_app_kit::{NSWindow, NSWindowButton};
        // SAFETY: Tauri runs this on the main thread and owns the NSWindow
        // for the duration of the callback.
        let native = unsafe { &*webview.ns_window().cast::<NSWindow>() };
        native.setCollectionBehavior(collection_behavior(native.collectionBehavior()));
        for kind in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = native.standardWindowButton(kind) {
                button.setHidden(true);
            }
        }
    })?;
    let events = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Focused(false) => {
            // A press on the status item can steal focus before its click
            // event arrives; let that click's toggle close the panel instead
            // of hiding here and immediately reopening.
            let in_grace = BLUR_GRACE_UNTIL
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|until| Instant::now() < until);
            if !in_grace && !press_on_status_item(&events) {
                hide(&events);
            }
        }
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide(&events);
        }
        _ => {}
    });
    Ok(())
}

/// Keep Tauri's workspace flags but make the panel Stationary, so Show
/// Desktop / Mission Control do not sweep it away.
#[cfg(target_os = "macos")]
fn collection_behavior(
    current: objc2_app_kit::NSWindowCollectionBehavior,
) -> objc2_app_kit::NSWindowCollectionBehavior {
    use objc2_app_kit::NSWindowCollectionBehavior as B;
    (current & !(B::Managed | B::Transient))
        | B::Stationary
        | B::MoveToActiveSpace
        | B::FullScreenAuxiliary
}

/// Position, reveal and focus the panel.
fn show(app: &AppHandle) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return Ok(());
    };
    // Tray rect is physical pixels with a top-left origin.
    let anchor = app
        .tray_by_id(TRAY_ID)
        .and_then(|tray| tray.rect().ok().flatten())
        .map(|rect| {
            let position = rect.position.to_physical::<f64>(1.0);
            let size = rect.size.to_physical::<f64>(1.0);
            (
                position.x + size.width / 2.0,
                position.y + size.height / 2.0,
            )
        });
    // monitor_from_point expects logical points on macOS, so match the
    // physical anchor against physical monitor bounds ourselves.
    let monitor = match anchor {
        Some(point) => app.available_monitors()?.into_iter().find(|m| {
            contains(
                point,
                (
                    f64::from(m.position().x),
                    f64::from(m.position().y),
                    f64::from(m.size().width),
                    f64::from(m.size().height),
                ),
            )
        }),
        None => None,
    };
    let monitor = match monitor {
        Some(m) => Some(m),
        None => app.primary_monitor()?,
    };
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let scale = monitor.scale_factor();
        let work = (
            f64::from(area.position.x),
            f64::from(area.position.y),
            f64::from(area.size.width),
            f64::from(area.size.height),
        );
        let width = (WIDTH * scale).min(work.2);
        let height = (HEIGHT * scale).min(work.3);
        window.set_size(tauri::LogicalSize::new(width / scale, height / scale))?;
        let anchor = anchor.unwrap_or((work.0 + work.2 - width / 2.0, work.1));
        let (x, y) = position(anchor, work, (width, height), GAP * scale);
        window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))?;
    }
    window.show()?;
    let _ = app.emit_to(PANEL_LABEL, VISIBILITY_EVENT, true);
    seed(app);
    #[cfg(target_os = "macos")]
    window.with_webview(|webview| {
        use objc2_app_kit::NSWindow;
        // Make the panel key/main before activation so activating the app
        // does not bring the main window forward over it.
        // SAFETY: main thread, Tauri-owned NSWindow (see ensure_created).
        let native = unsafe { &*webview.ns_window().cast::<NSWindow>() };
        native.makeMainWindow();
        native.makeKeyAndOrderFront(None);
    })?;
    window.set_focus()?;
    Ok(())
}

/// Is a primary-button press currently on the status item?
fn press_on_status_item(app: &AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        // Bit 0 = primary button. A parked pointer alone must not block
        // Cmd-Tab or another app taking focus.
        let pressed = objc2_app_kit::NSEvent::pressedMouseButtons() & 1 != 0;
        if !pressed {
            return false;
        }
        let Ok(cursor) = app.cursor_position() else {
            return false;
        };
        app.tray_by_id(TRAY_ID)
            .and_then(|tray| tray.rect().ok().flatten())
            .is_some_and(|rect| {
                let p = rect.position.to_physical::<f64>(1.0);
                let s = rect.size.to_physical::<f64>(1.0);
                contains((cursor.x, cursor.y), (p.x, p.y, s.width, s.height))
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

/// Point-in-rect for (x, y, width, height) bounds, right/bottom exclusive.
fn contains(point: (f64, f64), bounds: (f64, f64, f64, f64)) -> bool {
    point.0 >= bounds.0
        && point.0 < bounds.0 + bounds.2
        && point.1 >= bounds.1
        && point.1 < bounds.1 + bounds.3
}

/// Top-left of a panel of `size` centered horizontally on `anchor`, `gap`
/// below the top of the work area (i.e. under the menu bar), clamped so the
/// whole panel stays inside `area`. All values are physical pixels.
fn position(
    anchor: (f64, f64),
    area: (f64, f64, f64, f64),
    size: (f64, f64),
    gap: f64,
) -> (f64, f64) {
    let (left, top, width, height) = area;
    let x = (anchor.0 - size.0 / 2.0).clamp(left, left + (width - size.0).max(0.0));
    let y = (top + gap).clamp(top, top + (height - size.1).max(0.0));
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_centers_under_anchor_and_clamps_to_work_area() {
        // Retina main display, menu bar 74 px: centered, 12 px below.
        assert_eq!(
            position(
                (2000.0, 37.0),
                (0.0, 74.0, 3024.0, 1890.0),
                (780.0, 1220.0),
                12.0
            ),
            (1610.0, 86.0)
        );
        // Item near the right edge: clamped inside.
        assert_eq!(
            position(
                (3000.0, 37.0),
                (0.0, 74.0, 3024.0, 1890.0),
                (780.0, 1220.0),
                12.0
            ),
            (2244.0, 86.0)
        );
        // Secondary display with a negative origin.
        assert_eq!(
            position(
                (-100.0, 12.0),
                (-1920.0, 25.0, 1920.0, 1055.0),
                (390.0, 610.0),
                6.0
            ),
            (-390.0, 31.0)
        );
        // Work area shorter than the panel: pinned to the top.
        assert_eq!(
            position((0.0, 0.0), (0.0, 0.0, 300.0, 400.0), (300.0, 500.0), 6.0),
            (0.0, 0.0)
        );
    }

    #[test]
    fn contains_is_right_bottom_exclusive_and_handles_negative_origins() {
        assert!(contains((-100.0, 24.0), (-1920.0, 0.0, 1920.0, 1080.0)));
        assert!(!contains((0.0, 24.0), (-1920.0, 0.0, 1920.0, 1080.0)));
        assert!(!contains((10.0, -1.0), (0.0, 0.0, 100.0, 100.0)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn panel_is_stationary_and_keeps_workspace_flags() {
        use objc2_app_kit::NSWindowCollectionBehavior as B;
        let got = collection_behavior(B::Managed | B::FullScreenAuxiliary);
        assert!(got.contains(B::Stationary | B::MoveToActiveSpace | B::FullScreenAuxiliary));
        assert!(!got.intersects(B::Managed | B::Transient));
    }
}
