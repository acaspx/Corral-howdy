//! Corral desktop entry point.
// Prevents an extra console window on Windows release builds (not a target today; harmless on macOS).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    corral_desktop_lib::run();
}
