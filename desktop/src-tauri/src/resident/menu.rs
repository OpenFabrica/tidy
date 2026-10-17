// Right-click menu of the status item. The menu is attached to the
// NSStatusItem only while it is being shown: macOS 27 swallows primary
// clicks on a status item whose `menu` is permanently set (tray-icon #365),
// which would break left-click → panel.

use super::lifecycle;
use std::cell::RefCell;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIcon;
use tauri::{AppHandle, Wry};

/// Menu ids; route hashes ride along after the "resident-nav:" prefix.
const OPEN: &str = "resident-open";
const SETTINGS: &str = "resident-settings";
const QUIT: &str = "resident-quit";
const NAV: &str = "resident-nav:";

/// Main-window pages listed below a separator (route, zh, en).
const PAGES: [(&str, &str, &str); 5] = [
    ("#/clean", "清理", "Clean"),
    ("#/apps", "软件", "Apps"),
    ("#/optimize", "优化", "Optimize"),
    ("#/analyze", "分析", "Analyze"),
    ("#/status", "状态", "Status"),
];

thread_local! {
    /// Built menu and the language it was built for (main thread only).
    static MENU: RefCell<Option<(bool, Menu<Wry>)>> = const { RefCell::new(None) };
}

/// Show the menu under the status item. Runs on the main thread inside the
/// tray event callback; `performClick` tracks the menu synchronously, so
/// the menu is detached again as soon as it closes.
pub fn show(tray: &TrayIcon<Wry>) {
    let zh = prefers_chinese();
    let menu = MENU.with_borrow_mut(|cached| {
        if let Some((lang, menu)) = cached.as_ref() {
            if *lang == zh {
                return Some(menu.clone());
            }
        }
        let menu = build(tray.app_handle(), zh).ok()?;
        *cached = Some((zh, menu.clone()));
        Some(menu)
    });
    let Some(menu) = menu else {
        return;
    };
    if tray.set_menu(Some(menu)).is_err() {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = tray.with_inner_tray_icon(|inner| {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        if let Some(button) = inner.ns_status_item().and_then(|item| item.button(mtm)) {
            // SAFETY: main thread; the button belongs to the live status item.
            unsafe { button.performClick(None) };
        }
    });
    let _ = tray.set_menu(None::<Menu<Wry>>);
}

/// Build the localized menu.
fn build(app: &AppHandle, zh: bool) -> tauri::Result<Menu<Wry>> {
    let label = |zh_text: &str, en: &str| {
        if zh {
            zh_text.to_string()
        } else {
            en.to_string()
        }
    };
    let mut builder = MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id(OPEN, label("打开 Tidy", "Open Tidy")).build(app)?)
        .item(&MenuItemBuilder::with_id(SETTINGS, label("设置", "Settings")).build(app)?)
        .separator();
    for (hash, zh_text, en) in PAGES {
        builder = builder.item(
            &MenuItemBuilder::with_id(format!("{NAV}{hash}"), label(zh_text, en)).build(app)?,
        );
    }
    builder
        .separator()
        .item(&MenuItemBuilder::with_id(QUIT, label("退出", "Quit")).build(app)?)
        .build()
}

/// App-wide menu event handler; ignores ids that are not ours.
pub fn handle(app: &AppHandle, id: &str) {
    match id {
        OPEN => lifecycle::show_main(app, None),
        SETTINGS => lifecycle::show_main(app, Some("#/settings")),
        QUIT => app.exit(0),
        other => {
            if let Some(hash) = other.strip_prefix(NAV) {
                lifecycle::show_main(app, Some(hash));
            }
        }
    }
}

/// Whether the user's first preferred language is Chinese.
fn prefers_chinese() -> bool {
    #[cfg(target_os = "macos")]
    {
        let languages = objc2_foundation::NSLocale::preferredLanguages();
        languages
            .firstObject()
            .is_some_and(|first| first.to_string().starts_with("zh"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("LANG").unwrap_or_default().starts_with("zh")
    }
}
