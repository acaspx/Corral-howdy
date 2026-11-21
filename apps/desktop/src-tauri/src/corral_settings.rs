//! The main window's Corral settings commands (M4 item 2, TP-USAGE-15): which agents the corral
//! shows and whether its order stays fixed. The rules are the corral's order
//! ([`crate::corral_order`]); the running corral attaches a handle here and does the change, then
//! re-places its drops. Before the corral runs, the commands say so.

use crate::corral_order::CorralSettingsView;
use std::sync::{Arc, OnceLock};
use tauri::State;

/// What the running corral does for the settings. Each answer is the settings after it.
pub trait CorralControl: Send + Sync {
    /// The settings now.
    fn settings(&self) -> Result<CorralSettingsView, String>;
    /// Shows these agents (at most 8, the user's own), in the corral's order.
    fn choose_shown(&self, ids: Vec<String>) -> Result<CorralSettingsView, String>;
    /// Turns "Keep order fixed" on or off.
    fn keep_order_fixed(&self, keep: bool) -> Result<CorralSettingsView, String>;
}

/// The running corral, once it's attached.
#[derive(Default)]
pub struct CorralSettings {
    control: OnceLock<Arc<dyn CorralControl>>,
}

impl CorralSettings {
    /// Lets the settings reach the running corral. The first one attached stays.
    pub fn attach(&self, control: Arc<dyn CorralControl>) {
        if self.control.set(control).is_err() {
            eprintln!("corral: the Corral settings already reach a corral");
        }
    }

    fn control(&self) -> Result<&dyn CorralControl, String> {
        self.control
            .get()
            .map(AsRef::as_ref)
            .ok_or_else(|| "The corral isn't running yet, so its settings can't change.".to_owned())
    }

    /// The settings now.
    pub fn settings(&self) -> Result<CorralSettingsView, String> {
        self.control()?.settings()
    }

    /// Shows these agents.
    pub fn choose_shown(&self, ids: Vec<String>) -> Result<CorralSettingsView, String> {
        self.control()?.choose_shown(ids)
    }

    /// Turns "Keep order fixed" on or off.
    pub fn keep_order_fixed(&self, keep: bool) -> Result<CorralSettingsView, String> {
        self.control()?.keep_order_fixed(keep)
    }
}

// Async so a busy corral never blocks the main thread.

/// Command: the Corral settings.
#[tauri::command(async)]
pub fn corral_settings(
    settings: State<'_, Arc<CorralSettings>>,
) -> Result<CorralSettingsView, String> {
    settings.settings()
}

/// Command: which agents the corral shows.
#[tauri::command(async)]
pub fn set_corral_shown(
    settings: State<'_, Arc<CorralSettings>>,
    ids: Vec<String>,
) -> Result<CorralSettingsView, String> {
    settings.choose_shown(ids)
}

/// Command: "Keep order fixed".
#[tauri::command(async)]
pub fn set_keep_order_fixed(
    settings: State<'_, Arc<CorralSettings>>,
    keep: bool,
) -> Result<CorralSettingsView, String> {
    settings.keep_order_fixed(keep)
}
