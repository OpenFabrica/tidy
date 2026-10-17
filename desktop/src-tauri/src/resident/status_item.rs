// Native status-item painting. The whole item is one NSImage built with a
// drawing handler, so AppKit renders it at the screen's real backing scale
// (crisp on Retina, no PNG round trip) and it is never a template image, so
// tone colors survive. All AppKit work happens on the main thread; callers on
// other threads hand over plain data through `publish` / `set_logo`.

use super::columns::{self, Arrow, Body, Column};
use super::TRAY_ID;
use mole_ops::resident::prefs::Tone;
use tauri::AppHandle;

/// Everything the status item shows besides the logo animation frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Content {
    pub columns: Vec<Column>,
    pub icon: bool,
    pub compact: bool,
    /// Tooltip / accessibility text below "Tidy".
    pub summary: String,
}

/// Which image the logo slot shows: the static brand mark or one authored
/// axe frame while a long-running command holds a busy guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Logo {
    #[default]
    Idle,
    Axe(usize),
}

/// One change handed to the main thread.
enum Update {
    Content(Content),
    Logo(Logo),
    /// The native item was re-created (shown again); repaint unconditionally.
    Refresh,
}

/// Replace the drawn content. Cheap to call every tick: the main thread
/// repaints only when the content, logo, appearance or button changed.
pub fn publish(app: &AppHandle, content: Content) {
    dispatch(app, Update::Content(content));
}

/// Switch the logo slot (busy animation frames / idle).
pub fn set_logo(app: &AppHandle, logo: Logo) {
    dispatch(app, Update::Logo(logo));
}

/// Force one repaint (after the item becomes visible again).
pub fn refresh(app: &AppHandle) {
    dispatch(app, Update::Refresh);
}

/// Run `update` on the main thread against the live status item.
fn dispatch(app: &AppHandle, update: Update) {
    let target = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = target.tray_by_id(TRAY_ID) else {
            return;
        };
        // We are already on the main thread, so this runs synchronously.
        let _ = tray.with_inner_tray_icon(move |inner| native::apply(inner, update));
    });
}

/// Light / dark menu-bar palettes per tone. Darker hues stay legible on a
/// light bar, lighter ones on a dark bar; Normal follows the bar foreground.
pub fn tone_rgb(tone: Tone, dark: bool) -> [u8; 3] {
    match (tone, dark) {
        (Tone::Normal, false) => [0, 0, 0],
        (Tone::Normal, true) => [255, 255, 255],
        (Tone::Green, false) => [24, 120, 62],
        (Tone::Green, true) => [97, 213, 145],
        (Tone::Yellow, false) => [133, 101, 0],
        (Tone::Yellow, true) => [240, 207, 94],
        (Tone::Blue, false) => [27, 99, 190],
        (Tone::Blue, true) => [117, 180, 255],
        (Tone::Warning, false) => [166, 77, 0],
        (Tone::Warning, true) => [255, 190, 92],
        (Tone::Critical, false) => [194, 34, 42],
        (Tone::Critical, true) => [255, 112, 117],
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSAccessibility, NSAppearanceCustomization, NSAutoresizingMaskOptions, NSCellImagePosition,
        NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSImage,
        NSStringDrawing,
    };
    use objc2_foundation::{NSData, NSDictionary, NSPoint, NSRect, NSSize, NSString};
    use std::cell::RefCell;

    /// Main-thread render state: what is currently on screen plus the
    /// decoded logo images (NSImage is not Send, so it lives here).
    #[derive(Default)]
    struct State {
        content: Content,
        /// A first Content arrived; until then a logo-only update must not
        /// paint an empty (zero-width) item.
        has_content: bool,
        logo: Logo,
        /// Last painted (content, logo, appearance, button identity).
        painted: Option<(Content, Logo, String, usize)>,
        tooltip: Option<(String, usize)>,
        idle: Option<Retained<NSImage>>,
        axes: Vec<Retained<NSImage>>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    /// Merge one update and repaint the button image if anything visible changed.
    pub(super) fn apply(tray: &tray_icon::TrayIcon, update: Update) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        STATE.with_borrow_mut(|state| {
            let mut force = false;
            match update {
                Update::Content(content) => {
                    state.content = content;
                    state.has_content = true;
                }
                Update::Logo(logo) => state.logo = logo,
                Update::Refresh => force = true,
            }
            if !state.has_content {
                return;
            }
            let Some(item) = tray.ns_status_item() else {
                return;
            };
            let Some(button) = item.button(mtm) else {
                return;
            };
            let appearance = button.effectiveAppearance().name().to_string();
            let identity = Retained::as_ptr(&button) as usize;
            let key = (
                state.content.clone(),
                state.logo,
                appearance.clone(),
                identity,
            );
            if force || state.painted.as_ref() != Some(&key) {
                let logo = state.content.icon.then(|| logo_image(state)).flatten();
                let image = image(&state.content, logo, appearance.contains("Dark"));
                button.setTitle(&NSString::from_str(""));
                button.setImage(Some(&image));
                button.setImagePosition(NSCellImagePosition::ImageOnly);
                // NSVariableStatusItemLength: AppKit adds its usual padding.
                item.setLength(-1.0);
                track_button_size(&button);
                state.painted = Some(key);
            }
            let tooltip = format!("Tidy\n{}", state.content.summary)
                .trim_end()
                .to_string();
            if state.tooltip.as_ref() != Some(&(tooltip.clone(), identity)) {
                let text = NSString::from_str(&tooltip);
                button.setToolTip(Some(&text));
                button.setAccessibilityLabel(Some(&text));
                state.tooltip = Some((tooltip, identity));
            }
        });
    }

    /// tray-icon routes clicks through a transparent subview sized to the
    /// button at creation time. Our image changes the button width, so make
    /// that view follow the button or clicks on the new area are lost.
    fn track_button_size(button: &objc2_app_kit::NSStatusBarButton) {
        let bounds = button.bounds();
        for view in button.subviews().iter() {
            if view.class().name().to_str() == Ok("TaoTrayTarget") {
                view.setAutoresizingMask(
                    NSAutoresizingMaskOptions::ViewWidthSizable
                        | NSAutoresizingMaskOptions::ViewHeightSizable,
                );
                view.setFrame(bounds);
            }
        }
    }

    /// Decoded logo for the current state (idle mark or one axe frame).
    fn logo_image(state: &mut State) -> Option<Retained<NSImage>> {
        match state.logo {
            Logo::Idle => {
                if state.idle.is_none() {
                    state.idle = decode(crate::tray_anim::IDLE_PNG);
                }
                state.idle.clone()
            }
            Logo::Axe(frame) => {
                if state.axes.is_empty() {
                    state.axes = crate::tray_anim::AXE_PNGS
                        .iter()
                        .filter_map(|png| decode(png))
                        .collect();
                }
                if state.axes.is_empty() {
                    return state.idle.clone();
                }
                let index = frame % state.axes.len();
                Some(state.axes[index].clone())
            }
        }
    }

    /// PNG bytes → NSImage (keeps the 44 px bitmap; drawn into an 18 pt slot).
    fn decode(png: &[u8]) -> Option<Retained<NSImage>> {
        NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))
    }

    /// Point rect in the flipped (top-left origin) drawing context.
    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    /// sRGB color from 8-bit components.
    fn rgb([r, g, b]: [u8; 3]) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
            1.0,
        )
    }

    /// Draw one string with monospaced digits; `right` aligns it to the
    /// rect's right edge (digits grow leftward). Returns the measured width.
    fn text(
        value: &str,
        mut area: NSRect,
        size: f64,
        weight: f64,
        color: &NSColor,
        right: bool,
    ) -> f64 {
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
        // SAFETY: the font/color attribute keys require an NSFont and an
        // NSColor value respectively, which is exactly what is passed.
        let attributes: Retained<NSDictionary<NSString, AnyObject>> = unsafe {
            NSDictionary::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[&*font, color],
            )
        };
        let value = NSString::from_str(value);
        // SAFETY: attributes holds valid AppKit string-drawing attributes.
        unsafe {
            let width = value.sizeWithAttributes(Some(&attributes)).width;
            if right {
                let offset = (area.size.width - width).max(0.0);
                area.origin.x += offset;
                area.size.width -= offset;
            }
            value.drawInRect_withAttributes(area, Some(&attributes));
            width
        }
    }

    /// Arrow glyph and its fixed color (red up, blue down) — direction stays
    /// explicit for users who cannot tell the colors apart.
    fn arrow(direction: Arrow) -> (&'static str, Retained<NSColor>) {
        match direction {
            Arrow::Up => ("↑", NSColor::systemRedColor()),
            Arrow::Down => ("↓", NSColor::systemBlueColor()),
        }
    }

    /// Build the drawing-handler image for `content`.
    fn image(content: &Content, logo: Option<Retained<NSImage>>, dark: bool) -> Retained<NSImage> {
        let icon = content.icon && logo.is_some();
        let size = NSSize::new(
            f64::from(columns::width(&content.columns, icon, content.compact)),
            columns::HEIGHT,
        );
        let cols = content.columns.clone();
        let compact = content.compact;
        let draw = RcBlock::new(move |_: NSRect| {
            let foreground = rgb(tone_rgb(Tone::Normal, dark));
            let gap = f64::from(columns::gap(compact));
            let mut x = 0.0;
            if let Some(logo) = &logo {
                let edge = f64::from(columns::LOGO);
                logo.drawInRect(rect(0.0, (columns::HEIGHT - edge) / 2.0, edge, edge));
                x = edge + gap;
            }
            for column in &cols {
                let width = f64::from(column.width);
                match &column.body {
                    Body::Stacked {
                        heading,
                        arrow: direction,
                        value,
                        tone,
                    } => {
                        let heading_width = text(
                            heading,
                            rect(x, 0.0, width, 9.0),
                            7.5,
                            0.0,
                            &foreground,
                            false,
                        );
                        if let Some(direction) = direction {
                            let (glyph, color) = arrow(*direction);
                            text(
                                glyph,
                                rect(x + heading_width + 1.0, 0.0, 8.0, 9.0),
                                7.5,
                                0.0,
                                &color,
                                false,
                            );
                        }
                        let size = if compact { 11.0 } else { 12.0 };
                        text(
                            value,
                            rect(x, 8.0, width, 14.0),
                            size,
                            0.3,
                            &rgb(tone_rgb(*tone, dark)),
                            false,
                        );
                    }
                    Body::Rows(rows) => {
                        let size = if compact { 10.0 } else { 11.0 };
                        for ((label, tone), y) in rows.iter().zip([0.0, 11.0]) {
                            text(
                                label,
                                rect(x, y, width, 11.0),
                                size,
                                0.0,
                                &rgb(tone_rgb(*tone, dark)),
                                false,
                            );
                        }
                    }
                    Body::Rates { up, down } => {
                        let layout = columns::rate_layout(compact);
                        for ((value, unit), direction, y) in
                            [(up, Arrow::Up, 0.0), (down, Arrow::Down, 11.0)]
                        {
                            let (glyph, color) = arrow(direction);
                            text(glyph, rect(x, y, 10.0, 11.0), 10.0, 0.3, &color, false);
                            let value_rect = rect(
                                x + f64::from(layout.value_offset),
                                y,
                                f64::from(layout.value_width),
                                11.0,
                            );
                            text(value, value_rect, 10.0, 0.0, &foreground, true);
                            let unit_rect = rect(
                                x + f64::from(layout.unit_offset),
                                y,
                                f64::from(layout.unit_width),
                                11.0,
                            );
                            text(unit, unit_rect, 10.0, 0.0, &foreground, false);
                        }
                    }
                }
                x += width + gap;
            }
            Bool::YES
        });
        let image = NSImage::imageWithSize_flipped_drawingHandler(size, true, &draw);
        image.setTemplate(false);
        image
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use mole_ops::resident::prefs::ResidentPrefs;
        use mole_ops::resident::{MetricId, ResidentReading};

        /// Width AppKit needs for `value` at the given font.
        fn measured(value: &str, size: f64, weight: f64) -> f64 {
            let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
            let attributes: Retained<NSDictionary<NSString, AnyObject>> =
                unsafe { NSDictionary::from_slices(&[NSFontAttributeName], &[&*font]) };
            unsafe {
                NSString::from_str(value)
                    .sizeWithAttributes(Some(&attributes))
                    .width
            }
        }

        /// Manual visual check of both palettes without toggling the system:
        /// cargo test -p tidy render_preview -- --ignored, then open
        /// /tmp/tidy-status-{light,dark}.tiff.
        #[test]
        #[ignore]
        fn render_preview() {
            let mut reading = ResidentReading::default();
            reading.cpu.usage = Some(12.0);
            reading.memory.usage_percent = Some(75.0);
            reading.disk.used_percent = Some(93.0);
            reading.cpu.temp_c = Some(48.0);
            reading.network.tx_bps = Some(2_400);
            reading.network.rx_bps = Some(3_500_000);
            reading.power.battery_watts = Some(9.6);
            let mut prefs = ResidentPrefs::default();
            for entry in &mut prefs.metrics {
                entry.enabled = matches!(
                    entry.id,
                    MetricId::Cpu
                        | MetricId::Memory
                        | MetricId::Disk
                        | MetricId::CpuTemperature
                        | MetricId::Network
                        | MetricId::BatteryPower
                );
            }
            let columns = columns::build(&reading, &prefs, &mut Default::default());
            let content = Content {
                summary: columns::summary(&columns),
                columns,
                icon: true,
                compact: false,
            };
            for (name, dark) in [("light", false), ("dark", true)] {
                let image = image(&content, decode(crate::tray_anim::IDLE_PNG), dark);
                let data = image.TIFFRepresentation().expect("tiff");
                let path = NSString::from_str(&format!("/tmp/tidy-status-{name}.tiff"));
                assert!(data.writeToFile_atomically(&path, true));
            }
        }

        /// Real AppKit metrics catch overflow that string-length tests miss.
        #[test]
        fn fixed_fields_fit_their_widest_values() {
            for compact in [false, true] {
                let size = if compact { 11.0 } else { 12.0 };
                let prefs = ResidentPrefs {
                    compact,
                    ..ResidentPrefs::default()
                };
                let cols =
                    columns::build(&ResidentReading::default(), &prefs, &mut Default::default());
                let usage = f64::from(cols[0].width);
                assert!(
                    measured("100%", size, 0.3) <= usage,
                    "100% overflows (compact={compact})"
                );
                let temp = if compact { 37.0 } else { 46.0 };
                assert!(measured("150°C", size, 0.3) <= temp);
                let power = if compact { 34.0 } else { 37.0 };
                assert!(measured("999W", size, 0.3) <= power);
                assert!(measured("<1W", size, 0.3) <= power);
                let fan = if compact { 38.0 } else { 42.0 };
                assert!(measured("9999", size, 0.3) <= fan);
                let layout = columns::rate_layout(compact);
                assert!(measured("999", 10.0, 0.0) <= f64::from(layout.value_width));
                for unit in if compact {
                    ["B", "K", "M", "G", "T"]
                } else {
                    ["B/s", "KB/s", "MB/s", "GB/s", "TB/s"]
                } {
                    assert!(
                        measured(unit, 10.0, 0.0) <= f64::from(layout.unit_width),
                        "{unit}"
                    );
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::*;

    /// Non-macOS builds keep the static tray icon.
    pub(super) fn apply<T>(_tray: &T, _update: Update) {}
}
