//! Starts the corral (CorralNative) from the shell (ADR-001). SK-2a shows an empty corral; SK-2b
//! streams the supervisor's roster and run snapshots through here and hands intents to the core.

use crate::bridge::{BridgeError, Intent, RosterSnapshot};
use crate::native;

/// Shows the corral panel with an empty roster. Called from Tauri's `setup`, on the main thread.
pub fn start() -> Result<(), native::NativeError> {
    native::start(on_intent)?;
    native::set_roster(&RosterSnapshot::ranked(Vec::new()))
}

/// Swift reports what the user did and never decides (AGENTS.md rule 3); this shell doesn't
/// decide either. Until SK-2b passes intents to `corral-core`, they are only reported.
fn on_intent(result: Result<Intent, BridgeError>) {
    match result {
        Ok(intent) => eprintln!("corral: intent {intent:?} (handled from SK-2b)"),
        Err(e) => eprintln!("corral: refused an intent from CorralNative: {e}"),
    }
}
