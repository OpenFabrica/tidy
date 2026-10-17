// Menu-bar busy animation, driven by one persistent thread:
//   idle — the static brand mark in the status item's logo slot; the thread
//          only polls an atomic and never touches the main thread;
//   busy — the pre-rendered axe-strike frames in the same logo slot while any
//          command holds a BusyGuard.
// The status item paints the frame into its drawing-handler image (see
// resident::status_item), so a frame swap is one cheap main-thread repaint
// with no PNG encoding; metric columns keep showing next to it.

use crate::resident::status_item::{self, Logo};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tauri::AppHandle;

/// Runtime frame contract: 44px canvas (22pt menu bar @2x), 14 authored poses
/// at ~11 fps. The images are decoded once on the main thread and the loop
/// only selects frame indices; it never rotates, scales, or paints the axe.
#[cfg(test)]
const TRAY_FRAME_SIZE: u32 = 44;
const AXE_FRAMES: usize = 14;
const TICK_MS: u64 = 90;

/// Static brand mark shown in the logo slot (also the tray's initial icon).
pub const IDLE_PNG: &[u8] = include_bytes!("../icons/tray-axe/idle.png");

/// The authored axe sequence. A single animated GIF cannot be used: the
/// status item displays one decoded image, so frames are swapped explicitly.
pub const AXE_PNGS: [&[u8]; AXE_FRAMES] = [
    include_bytes!("../icons/tray-axe/axe-00.png"),
    include_bytes!("../icons/tray-axe/axe-01.png"),
    include_bytes!("../icons/tray-axe/axe-02.png"),
    include_bytes!("../icons/tray-axe/axe-03.png"),
    include_bytes!("../icons/tray-axe/axe-04.png"),
    include_bytes!("../icons/tray-axe/axe-05.png"),
    include_bytes!("../icons/tray-axe/axe-06.png"),
    include_bytes!("../icons/tray-axe/axe-07.png"),
    include_bytes!("../icons/tray-axe/axe-08.png"),
    include_bytes!("../icons/tray-axe/axe-09.png"),
    include_bytes!("../icons/tray-axe/axe-10.png"),
    include_bytes!("../icons/tray-axe/axe-11.png"),
    include_bytes!("../icons/tray-axe/axe-12.png"),
    include_bytes!("../icons/tray-axe/axe-13.png"),
];

/// Number of in-flight long-running commands; > 0 selects axe animation mode.
static BUSY: AtomicUsize = AtomicUsize::new(0);

/// Start the animation thread. Before this is called busy guards are
/// harmless no-ops.
pub fn init(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || run_animation(app));
}

/// RAII busy marker held by long-running commands for their whole lifetime,
/// so the counter stays balanced on every exit path — including `?` errors.
pub struct BusyGuard(());

/// Mark a long-running task as active; the axe animation shows while any guard lives.
pub fn busy() -> BusyGuard {
    BUSY.fetch_add(1, Ordering::AcqRel);
    BusyGuard(())
}

impl Drop for BusyGuard {
    // The animation thread notices the 0 count on its next tick and restores
    // the static brand mark; nothing else to do here.
    fn drop(&mut self) {
        BUSY.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The two animation states.
#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Idle,
    Busy,
}

/// Persistent frame loop. `status_item::set_logo` hops to the main thread
/// itself, so a plain background thread with sleep is safe here.
fn run_animation(app: AppHandle) {
    let mut mode = Mode::Idle;
    let mut frame = 0usize; // authored axe frame index
    loop {
        let busy = BUSY.load(Ordering::Acquire) > 0;
        match (busy, mode) {
            (true, Mode::Idle) => {
                // Grace period: a command that finishes this fast never
                // flips the icon, so cache hits cause no axe-animation flash.
                std::thread::sleep(Duration::from_millis(150));
                if BUSY.load(Ordering::Acquire) > 0 {
                    mode = Mode::Busy;
                    frame = 0;
                }
            }
            (true, Mode::Busy) => {
                status_item::set_logo(&app, Logo::Axe(frame % AXE_FRAMES));
                frame += 1;
                std::thread::sleep(Duration::from_millis(TICK_MS));
            }
            (false, Mode::Busy) => {
                mode = Mode::Idle;
                // Restore the brand mark once on the state transition.
                status_item::set_logo(&app, Logo::Idle);
            }
            (false, Mode::Idle) => {
                // Poll only the atomic busy counter: an idle status item gets
                // no main-thread work from this thread.
                std::thread::sleep(Duration::from_millis(TICK_MS));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::image::Image;

    #[test]
    fn authored_axe_frames_match_the_menu_bar_contract() {
        let idle = Image::from_bytes(IDLE_PNG).unwrap();
        assert_eq!(
            (idle.width(), idle.height()),
            (TRAY_FRAME_SIZE, TRAY_FRAME_SIZE)
        );
        for png in AXE_PNGS {
            let frame = Image::from_bytes(png).unwrap();
            assert_eq!(
                (frame.width(), frame.height()),
                (TRAY_FRAME_SIZE, TRAY_FRAME_SIZE)
            );
        }
    }
}
