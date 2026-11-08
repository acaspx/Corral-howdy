//! Corral's Tauri shell (M0 skeleton): a main window and a menu-bar icon.
//!
//! This crate stays thin by rule (`AGENTS.md` rule 3): windows, tray, hotkeys and IPC wiring only.
//! Lifecycle, policy and budget decisions live in `corral-core` / `corral-policy`. The liquid-glass
//! corral itself is native Swift (`native/CorralNative`), linked in after Spike S1.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WindowEvent};

const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";

/// Corral supervises running agents, so it keeps running when its last window closes: closing the
/// main window hides it, and only an explicit Quit (which exits with a code) ends the app.
fn should_keep_running(exit_code: Option<i32>) -> bool {
    exit_code.is_none()
}

/// Builds and runs the app.
pub fn run() {
    let app = tauri::Builder::default()
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
                            // Best effort: if the window can't be shown, the tray stays usable and
                            // nothing is lost. There is no safer fallback to report to.
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
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                // Best effort: closing is already prevented, so a failed hide leaves the window up.
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("Corral failed to start");

    app.run(|_app, event| {
        if let RunEvent::ExitRequested { code, api, .. } = event
            && should_keep_running(code)
        {
            api.prevent_exit();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tp_shell_01_closing_the_last_window_keeps_corral_running() {
        // Window close / Cmd-W asks to exit with no code: keep running in the menu bar.
        assert!(should_keep_running(None));
    }

    #[test]
    fn tp_shell_02_quit_from_the_menu_really_quits() {
        assert!(!should_keep_running(Some(0)));
    }

    #[test]
    fn tp_shell_03_csp_blocks_base_form_and_framing() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap();
        for directive in [
            "default-src 'self'",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(
                csp.contains(directive),
                "CSP is missing `{directive}`: {csp}"
            );
        }
    }
}
