//! Connects the core to the corral (CorralNative, ADR-001).
//!
//! The core decides what the corral shows ([`CorralTracker`], SK-2b); this module only relays:
//! [`CorralFeed::watch`] wraps the event log so every event the log accepts updates the tracker,
//! and each change is sent to Swift on the main thread. Intents from Swift are reported here until
//! SK-6 hands them to `corral-core`, which decides (AGENTS.md rule 3).

use crate::bridge::{BridgeError, Intent, RosterSnapshot};
use crate::native;
use corral_core::corral_view::{AgentCard, CorralTracker};
use corral_core::event::Event;
use corral_core::supervisor::{EventLog, WatchedLog};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

/// Shows the corral and returns the feed that keeps it current. Called from Tauri's `setup`, on
/// the main thread. Until agents can be added (SK-3, SK-4) the corral starts empty.
pub fn start(
    app: &AppHandle,
    agents: Vec<AgentCard>,
) -> Result<Arc<CorralFeed>, native::NativeError> {
    native::start(on_intent)?;
    let feed = Arc::new(CorralFeed {
        tracker: Mutex::new(CorralTracker::new(agents)),
        app: app.clone(),
    });
    native::set_roster(&RosterSnapshot::from(&feed.current()?))?;
    Ok(feed)
}

/// Keeps the corral in step with the event log.
pub struct CorralFeed {
    tracker: Mutex<CorralTracker>,
    app: AppHandle,
}

impl CorralFeed {
    /// Wraps `log` so every event it accepts updates the corral. Give the result to the
    /// supervisor as its event log.
    pub fn watch(self: &Arc<Self>, log: Arc<dyn EventLog>) -> Arc<dyn EventLog> {
        let feed = Arc::clone(self);
        Arc::new(WatchedLog::new(log, move |event: &Event| {
            feed.observe(event)
        }))
    }

    fn current(&self) -> Result<corral_core::corral_view::CorralView, native::NativeError> {
        self.tracker
            .lock()
            .map(|t| t.view())
            .map_err(|_| native::NativeError::Poisoned)
    }

    /// Updates the tracker and, when a drop changed, sends the new roster to Swift. The roster is
    /// queued for the main thread while the tracker is still locked, so updates reach Swift in
    /// the order they happened.
    fn observe(&self, event: &Event) {
        let Ok(mut tracker) = self.tracker.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        match tracker.observe(event) {
            Ok(false) => {}
            Ok(true) => {
                let roster = RosterSnapshot::from(&tracker.view());
                let posted = self.app.run_on_main_thread(move || {
                    if let Err(e) = native::set_roster(&roster) {
                        eprintln!("corral: couldn't update the corral: {e}");
                    }
                });
                if let Err(e) = posted {
                    eprintln!("corral: couldn't reach the main thread to update the corral: {e}");
                }
            }
            // The supervisor already validated the event; a refusal here means the two diverged.
            Err(e) => eprintln!("corral: the corral view refused {:?}: {e}", event.kind()),
        }
    }
}

/// Swift reports what the user did and never decides (AGENTS.md rule 3); this shell doesn't
/// decide either. Until SK-6 passes intents to `corral-core`, they are only reported.
fn on_intent(result: Result<Intent, BridgeError>) {
    match result {
        Ok(intent) => eprintln!("corral: intent {intent:?} (handled from SK-6)"),
        Err(e) => eprintln!("corral: refused an intent from CorralNative: {e}"),
    }
}
