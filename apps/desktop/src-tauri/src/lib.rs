//! Corral's Tauri shell (M0 skeleton): a main window and a menu-bar icon.
//!
//! This crate stays thin by rule (`AGENTS.md` rule 3): windows, tray, hotkeys and IPC wiring only.
//! Lifecycle, policy and budget decisions live in `corral-core` / `corral-policy`. The liquid-glass
//! corral itself is native Swift (`native/CorralNative`), linked in after Spike S1.

use tauri::Manager;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;

const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";

/// Builds and runs the app.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let show = MenuItem::with_id(app, MENU_SHOW, "Show Corral", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Corral", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("Corral: no app icon configured in tauri.conf.json")?;

            TrayIconBuilder::with_id("corral")
                .icon(icon)
                .icon_as_template(true)
                .tooltip("Corral")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    MENU_SHOW => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    MENU_QUIT => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Corral failed to start");
}
