//! Corral's Tauri shell (M0 skeleton): a main window and a menu-bar icon.
//!
//! This crate stays thin by rule (`AGENTS.md` rule 3): windows, tray, hotkeys and IPC wiring only.
//! Lifecycle, policy and budget decisions live in `corral-core` / `corral-policy`. The liquid-glass
//! corral itself is native Swift (`native/CorralNative`), linked over a C ABI (ADR-001).

// The bridge to CorralNative (ADR-001). `bridge` is the pure wire contract, tested on every
// platform; `native` (the FFI) and `surface` only build on macOS with `--features native`.
#[cfg(any(test, all(target_os = "macos", feature = "native")))]
pub mod bridge;
#[cfg(all(target_os = "macos", feature = "native"))]
#[allow(unsafe_code)] // the C ABI; see the module docs and docs/adr/001-stack.md
pub mod native;
#[cfg(all(target_os = "macos", feature = "native"))]
mod surface;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";

/// App-level events the shell reacts to, kept free of Tauri types so the decisions can be tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellEvent {
    /// The user closed the main window (red button or Command-W).
    MainWindowCloseRequested,
    /// The app is about to exit: Quit in the menu bar or app menu, Command-Q, logout or shutdown.
    ExitRequested,
    /// The Dock icon was clicked while the app was running (macOS "reopen"). Only macOS sends
    /// it; the tests still cover it on every platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Reopen { has_visible_windows: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellAction {
    HideMainWindow,
    AllowExit,
    ShowMainWindow,
    Nothing,
}

/// Corral supervises running agents, so closing its window must not quit it: the window hides and
/// the menu-bar icon stays. Because the window is hidden rather than destroyed, the app never runs
/// out of windows, so every exit request is a real quit and is always allowed. Preventing exits
/// here would risk blocking Command-Q, logout and shutdown.
fn decide(event: ShellEvent) -> ShellAction {
    match event {
        ShellEvent::MainWindowCloseRequested => ShellAction::HideMainWindow,
        ShellEvent::ExitRequested => ShellAction::AllowExit,
        ShellEvent::Reopen {
            has_visible_windows: false,
        } => ShellAction::ShowMainWindow,
        ShellEvent::Reopen {
            has_visible_windows: true,
        } => ShellAction::Nothing,
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Best effort: if the window can't be shown, the tray stays usable and nothing is lost.
        // There is no safer fallback to report to.
        let _ = window.show();
        let _ = window.set_focus();
    }
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
                    MENU_SHOW => show_main_window(app),
                    MENU_QUIT => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            #[cfg(all(target_os = "macos", feature = "native"))]
            surface::start()?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && decide(ShellEvent::MainWindowCloseRequested) == ShellAction::HideMainWindow
            {
                api.prevent_close();
                // Best effort: closing is already prevented, so a failed hide leaves the window up.
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("Corral failed to start");

    app.run(
        |#[cfg_attr(not(target_os = "macos"), allow(unused_variables))] app, event| match event {
            // Nothing to do: `decide` always allows a real exit (see its doc comment).
            RunEvent::ExitRequested { .. } => {
                debug_assert_eq!(decide(ShellEvent::ExitRequested), ShellAction::AllowExit);
            }
            #[cfg(all(target_os = "macos", feature = "native"))]
            RunEvent::Exit => {
                if let Err(e) = native::stop() {
                    eprintln!("corral: couldn't hide the corral on exit: {e}");
                }
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen {
                has_visible_windows,
                ..
            } if decide(ShellEvent::Reopen {
                has_visible_windows,
            }) == ShellAction::ShowMainWindow =>
            {
                show_main_window(app);
            }
            _ => {}
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tp_shell_01_closing_the_last_window_keeps_corral_running() {
        // The window hides instead of closing, so the app keeps running in the menu bar.
        assert_eq!(
            decide(ShellEvent::MainWindowCloseRequested),
            ShellAction::HideMainWindow
        );
    }

    #[test]
    fn tp_shell_02_quit_from_the_menu_really_quits() {
        // The tray's Quit calls `app.exit(0)`, which arrives as an exit request.
        assert_eq!(decide(ShellEvent::ExitRequested), ShellAction::AllowExit);
    }

    #[test]
    fn tp_shell_04_command_q_quits() {
        // Command-Q and Quit in the app menu also end in an exit request (or a direct terminate).
        assert_eq!(decide(ShellEvent::ExitRequested), ShellAction::AllowExit);
    }

    #[test]
    fn tp_shell_05_logout_and_shutdown_are_never_blocked() {
        // The shell has no way to say "prevent" for any exit request.
        assert_ne!(decide(ShellEvent::ExitRequested), ShellAction::Nothing);
        assert_eq!(decide(ShellEvent::ExitRequested), ShellAction::AllowExit);
    }

    #[test]
    fn tp_shell_06_dock_click_reopens_the_hidden_window() {
        assert_eq!(
            decide(ShellEvent::Reopen {
                has_visible_windows: false
            }),
            ShellAction::ShowMainWindow
        );
        assert_eq!(
            decide(ShellEvent::Reopen {
                has_visible_windows: true
            }),
            ShellAction::Nothing
        );
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
