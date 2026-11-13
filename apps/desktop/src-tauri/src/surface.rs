//! Connects the core to the corral (CorralNative, ADR-001).
//!
//! The core decides everything the corral shows and does (AGENTS.md rule 3); this module only
//! relays:
//! - **Out to Swift:** [`CorralFeed::watch`] wraps the event log, so every event the log accepts
//!   updates the corral view and the run cards ([`CorralTracker`], [`RunCards`]). Each change, and
//!   a tick once a second while a card's clock is moving, sends the roster with its cards to
//!   Swift on the main thread.
//! - **In from Swift:** a task typed into a drop, Stop, Send back and Retry go to
//!   [`CorralControls`], which decides. Other intents are reported until their milestones.
//!
//! The corral holds one agent per connected provider (Grok, Claude, GPT, Muse Spark; see
//! `corral_adapters::catalog`): connecting a key adds its drop and removing it takes the drop away
//! ([`Corral::refresh`]). Grok and Claude have clients; the others say so when given a task. No
//! tools, and a hard cap of [`RUN_CAP_MICRO_USD`] per run. Events go to the SQLite event store in
//! the app's data folder.
//!
//! Each connected provider's logo is fetched once from its own site and kept in the app's
//! `logos` folder ([`crate::logos`], ADR-015); the drop shows it from then on.

use crate::bridge::{BridgeError, Intent, RosterSnapshot};
use crate::keys::Keys;
use crate::logos::{LogoStore, fetch_missing};
use crate::native;
use corral_adapters::catalog::{AgentRouter, agent_for, agents_for};
use corral_adapters::logos::{LogoId, ReqwestLogoFetcher};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_adapters::transport::{ReqwestTransport, Transport};
use corral_core::budget::BudgetConfig;
use corral_core::clock::{Clock, SystemClock};
use corral_core::controls::{ControlError, CorralControls};
use corral_core::corral_view::{AgentCard, CorralTracker};
use corral_core::event::Event;
use corral_core::ids::AgentId;
use corral_core::run_card::RunCards;
use corral_core::supervisor::{EventLog, RunDriver, Supervisor, SupervisorConfig, WatchedLog};
use corral_secrets::{Provider, SecretStore};
use corral_store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// The skeleton's per-run hard cap: $0.25 (docs/03, J0).
pub const RUN_CAP_MICRO_USD: u64 = 250_000;
/// The model Grok's drop uses. Model IDs are config (docs/01 §4); this is the skeleton's.
pub const GROK_MODEL: &str = "grok-4.7";
/// The model Claude's drop uses (SK-7).
pub const CLAUDE_MODEL: &str = "claude-sonnet-5-5";

/// The agents for the providers with a key in the Keychain. A key that can't be read counts as
/// missing (the card would fail on it anyway) and is logged.
fn agents(store: &dyn SecretStore) -> Vec<AgentCard> {
    agents_for(&connected(store))
}

/// The providers with a key in the Keychain.
fn connected(store: &dyn SecretStore) -> Vec<Provider> {
    Provider::ALL
        .into_iter()
        .filter(|p| match corral_secrets::status(store, *p) {
            Ok(status) => status.configured,
            Err(e) => {
                eprintln!("corral: couldn't read the {} key: {e}", p.display_name());
                false
            }
        })
        .collect()
}

/// What `setup` keeps alive for the life of the app.
pub struct Corral {
    /// Keeps the corral in step with the log.
    pub feed: Arc<CorralFeed>,
    /// Turns clicks into runs, once the event store and the provider client are ready.
    pub controls: Option<Arc<CorralControls>>,
    /// Provider logos, when the app has a data folder.
    pub logos: Option<LogoStore>,
}

impl Corral {
    /// Brings the corral in line with the keys in `store`: a newly connected provider's agent
    /// drops in, and a removed one leaves (TP-VIEW-06). Called after a key is connected or removed.
    pub fn refresh(&self, store: &dyn SecretStore) {
        let cards = agents(store);
        if let Some(controls) = &self.controls {
            controls.set_agents(cards.iter().map(|c| c.id.clone()));
        }
        self.feed.set_agents(cards);
        self.show_logos(store);
    }

    /// Shows each connected provider's logo: a saved one at once, a missing one once it's
    /// fetched, off the main thread. A logo that can't be had is logged; the drop keeps its
    /// monogram.
    pub fn show_logos(&self, store: &dyn SecretStore) {
        let Some(logos) = self.logos.clone() else {
            return;
        };
        let wanted: Vec<(LogoId, AgentId)> = connected(store)
            .into_iter()
            .map(|p| (LogoId::Provider(p), agent_for(p).id))
            .collect();
        let feed = Arc::clone(&self.feed);
        // The task reports its own outcome, so its handle is dropped.
        tauri::async_runtime::spawn(async move {
            let ids: Vec<LogoId> = wanted.iter().map(|(id, _)| *id).collect();
            for (id, result) in fetch_missing(&ReqwestLogoFetcher, &logos, &ids).await {
                let Some((_, agent)) = wanted.iter().find(|(w, _)| *w == id) else {
                    continue;
                };
                match result.map(|path| path.into_os_string().into_string()) {
                    Ok(Ok(path)) => feed.set_logo(agent.clone(), path),
                    Ok(Err(path)) => eprintln!(
                        "corral: the {} logo's path isn't UTF-8: {}",
                        id.display_name(),
                        path.to_string_lossy()
                    ),
                    Err(e) => eprintln!("corral: no {} logo: {e}", id.display_name()),
                }
            }
        });
    }
}

/// Shows the corral and wires it to the core. Called from Tauri's `setup`, on the main thread.
/// If the event store or the provider client can't start, the corral still shows, and tasks are
/// refused with a logged reason.
pub fn start(app: &AppHandle) -> Result<Corral, native::NativeError> {
    let controls_slot: Arc<OnceLock<Arc<CorralControls>>> = Arc::new(OnceLock::new());
    let slot = Arc::clone(&controls_slot);
    native::start(move |intent| on_intent(&slot, intent))?;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let cards = agents(app.state::<Keys>().0.as_ref());
    let ids: Vec<AgentId> = cards.iter().map(|c| c.id.clone()).collect();
    let feed = Arc::new(CorralFeed {
        views: Mutex::new(Views {
            corral: CorralTracker::new(cards),
            cards: RunCards::default(),
            logos: HashMap::new(),
        }),
        app: app.clone(),
        clock: Arc::clone(&clock),
    });
    feed.publish_now()?;
    feed.start_ticking();

    let controls = match supervisor(app, &feed, clock) {
        Ok(supervisor) => {
            let controls = Arc::new(CorralControls::new(supervisor, ids, id_source()));
            // The slot is empty: this is its only writer, and `start` runs once.
            if controls_slot.set(Arc::clone(&controls)).is_err() {
                eprintln!("corral: the controls were already set; keeping the first");
            }
            Some(controls)
        }
        Err(reason) => {
            eprintln!("corral: tasks are off: {reason}");
            None
        }
    };
    let logos = match app.path().app_data_dir() {
        Ok(dir) => Some(LogoStore::new(dir.join("logos"))),
        Err(e) => {
            eprintln!("corral: no app data folder, so no provider logos: {e}");
            None
        }
    };
    let corral = Corral {
        feed,
        controls,
        logos,
    };
    corral.show_logos(app.state::<Keys>().0.as_ref());
    Ok(corral)
}

/// The supervisor: each agent's runs go to its provider (Grok and Claude have clients so far),
/// the event store, the $0.25 cap.
fn supervisor(
    app: &AppHandle,
    feed: &Arc<CorralFeed>,
    clock: Arc<dyn Clock>,
) -> Result<Supervisor, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data folder: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let store = Store::open(&dir.join("corral.db"))
        .map_err(|e| format!("couldn't open the event store: {e}"))?;
    if let Some(banner) = store.integrity().banner() {
        eprintln!("corral: {banner}");
    }
    match store.fail_orphaned_runs(clock.now_ms()) {
        Ok(failed) if failed.is_empty() => {}
        Ok(failed) => eprintln!("corral: marked {} unfinished runs as failed", failed.len()),
        Err(e) => eprintln!("corral: couldn't close out unfinished runs: {e}"),
    }
    let transport = ReqwestTransport::new().map_err(|e| format!("no HTTPS client: {e}"))?;
    let keys = app.state::<Keys>().0.clone();
    let transport: Arc<dyn Transport> = Arc::new(transport);
    let grok = ResponsesDriver::new(
        transport.clone(),
        keys.clone(),
        ResponsesConfig::xai(GROK_MODEL),
    );
    let claude = MessagesDriver::new(transport, keys, MessagesConfig::anthropic(CLAUDE_MODEL));
    let mut drivers: HashMap<AgentId, Arc<dyn RunDriver>> = HashMap::new();
    drivers.insert(agent_for(Provider::Xai).id, Arc::new(grok));
    drivers.insert(agent_for(Provider::Anthropic).id, Arc::new(claude));
    let driver = AgentRouter::new(drivers);
    Ok(Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: RUN_CAP_MICRO_USD,
        },
        Arc::new(driver),
        feed.watch(Arc::new(store)),
        clock,
    ))
}

/// Unique run and session ID suffixes: the start time plus a counter.
fn id_source() -> Arc<dyn Fn() -> String + Send + Sync> {
    let started = SystemClock.now_ms();
    let next = AtomicU64::new(1);
    Arc::new(move || format!("{started:x}-{}", next.fetch_add(1, Ordering::Relaxed)))
}

struct Views {
    corral: CorralTracker,
    cards: RunCards,
    /// Logo paths by agent.
    logos: HashMap<AgentId, String>,
}

/// Keeps the corral in step with the event log.
pub struct CorralFeed {
    views: Mutex<Views>,
    app: AppHandle,
    clock: Arc<dyn Clock>,
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

    /// Replaces the corral's agents and, if the corral changed, sends the new roster.
    fn set_agents(&self, cards: Vec<AgentCard>) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        if views.corral.set_agents(cards) {
            self.send(self.snapshot(&views));
        }
    }

    /// Shows `agent`'s logo from `path`, if it's new.
    fn set_logo(&self, agent: AgentId, path: String) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        if views.logos.get(&agent) != Some(&path) {
            views.logos.insert(agent, path);
            self.send(self.snapshot(&views));
        }
    }

    /// Sends the current roster from the main thread (during `setup`).
    fn publish_now(&self) -> Result<(), native::NativeError> {
        let views = self
            .views
            .lock()
            .map_err(|_| native::NativeError::Poisoned)?;
        native::set_roster(&self.snapshot(&views))
    }

    fn snapshot(&self, views: &Views) -> RosterSnapshot {
        RosterSnapshot::with_cards(&views.corral.view(), &views.cards, self.clock.now_ms())
            .with_logos(|id| views.logos.get(id).cloned())
    }

    /// Updates the views and, when a drop or a card changed, sends the new roster to Swift. The
    /// roster is queued for the main thread while the views are still locked, so updates reach
    /// Swift in the order they happened.
    fn observe(&self, event: &Event) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        // The supervisor already validated the event; a refusal here means the views diverged.
        let drop_changed = views.corral.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the corral view refused {:?}: {e}", event.kind());
            false
        });
        let card_changed = views.cards.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the run cards refused {:?}: {e}", event.kind());
            false
        });
        if drop_changed || card_changed {
            self.send(self.snapshot(&views));
        }
    }

    /// Once a second, while some card's clock is moving, sends the roster so elapsed time ticks
    /// (TP-SKEL-04). The thread lives as long as the app.
    fn start_ticking(self: &Arc<Self>) {
        let feed = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("corral-tick".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let Some(feed) = feed.upgrade() else { return };
                    let Ok(views) = feed.views.lock() else { return };
                    if views.cards.is_ticking() {
                        feed.send(feed.snapshot(&views));
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!(
                "corral: couldn't start the card clock; elapsed time updates on events only: {e}"
            );
        }
    }

    fn send(&self, roster: RosterSnapshot) {
        let posted = self.app.run_on_main_thread(move || {
            if let Err(e) = native::set_roster(&roster) {
                eprintln!("corral: couldn't update the corral: {e}");
            }
        });
        if let Err(e) = posted {
            eprintln!("corral: couldn't reach the main thread to update the corral: {e}");
        }
    }
}

/// Swift reports what the user did and never decides (AGENTS.md rule 3); this shell doesn't
/// decide either. Tasks, Stop, Send back and Retry go to the core's controls, off the main
/// thread. The rest are reported until their milestones.
fn on_intent(controls: &OnceLock<Arc<CorralControls>>, result: Result<Intent, BridgeError>) {
    let intent = match result {
        Ok(intent) => intent,
        Err(e) => {
            eprintln!("corral: refused an intent from CorralNative: {e}");
            return;
        }
    };
    let Some(controls) = controls.get().cloned() else {
        eprintln!("corral: tasks are off, so {intent:?} was dropped (see the startup log)");
        return;
    };
    // Each task's handle is dropped on purpose: the task reports its own outcome.
    match intent {
        Intent::DeployTask { agent, prompt } => {
            tauri::async_runtime::spawn(async move {
                report(
                    "deploy",
                    &agent,
                    controls.deploy(&agent, &prompt).map(|_| ()),
                );
            });
        }
        Intent::Stop(agent) => {
            tauri::async_runtime::spawn(async move {
                report("stop", &agent, controls.stop(&agent).await);
            });
        }
        Intent::SendBack(agent) => {
            tauri::async_runtime::spawn(async move {
                report("send back", &agent, controls.send_back(&agent));
            });
        }
        Intent::Retry(agent) => {
            tauri::async_runtime::spawn(async move {
                report("retry", &agent, controls.retry(&agent).map(|_| ()));
            });
        }
        Intent::OpenAgent(_)
        | Intent::Deploy(_)
        | Intent::SummonPromptBar
        | Intent::NotificationAction { .. }
        | Intent::NotificationFailed { .. }
        | Intent::AuthResult { .. } => {
            eprintln!("corral: intent {intent:?} (handled in a later milestone)");
        }
    }
}

/// Logs a refused click. The hover card shows these from SK-6b.
fn report(what: &str, agent: &AgentId, outcome: Result<(), ControlError>) {
    if let Err(e) = outcome {
        eprintln!("corral: {what} for {agent} refused: {e}");
    }
}
