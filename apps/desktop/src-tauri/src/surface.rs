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
//! ([`Corral::refresh`]). Grok and Claude have clients; the others say so when given a task.
//! Grok and Claude work with the file tools, each in its own folder (`~/Corral/Grok`,
//! `~/Corral/Claude`; [`crate::agent_tools`]): their calls go through the policy, and when the
//! policy asks, they wait on the agent's card.
//! Each run has a hard cap of [`RUN_CAP_MICRO_USD`] and asks on the card before going past
//! [`SOFT_LIMIT_MICRO_USD`]. Events go to the SQLite event store in the app's data folder.
//!
//! Each connected provider's logo is fetched once from its own site and kept in the app's
//! `logos` folder ([`crate::logos`], ADR-015); the drop shows it from then on.
//!
//! Linked apps (consumer Muse and your dot, ADR-005) are drops the user turns on from the
//! corral's right-click menu. They never take tasks: their card's Open button opens the app's
//! own site. The choice is kept in `linked-apps.json` ([`crate::linked`]).

use crate::agent_tools::{agent_folder, allowing_provider_tools, file_tools};
use crate::bridge::{AuthOutcome, BridgeError, Intent, RosterSnapshot};
use crate::corral_order::CorralOrder;
use crate::keys::Keys;
use crate::linked::LinkedStore;
use crate::logos::{LogoStore, fetch_missing};
use crate::native;
use crate::provider_tools::ProviderTools;
use crate::touch_id::{Finished, TouchIdDesk};
use crate::traces::Traces;
use corral_adapters::agent::{MessagesAgent, ProviderTool, ResponsesAgent, ToolSetup};
use corral_adapters::catalog::{AgentRouter, Switched, agent_for, agents_for, corral_agents};
use corral_adapters::linked::{LinkChange, LinkedApp, LinkedSettings};
use corral_adapters::logos::{LogoId, ReqwestLogoFetcher};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_adapters::transport::{ReqwestTransport, Transport};
use corral_core::budget::{BudgetConfig, SpendLedger};
use corral_core::clock::{Clock, SystemClock};
use corral_core::controls::{ControlError, CorralControls};
use corral_core::corral_view::{AgentCard, CorralTracker, DropState};
use corral_core::event::{Event, EventBody};
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::run_card::{EditError, RunCard, RunCards};
use corral_core::supervisor::{EventLog, RunDriver, Supervisor, SupervisorConfig, WatchedLog};
use corral_core::usage_rank::{Deploy, WINDOW_MS};
use corral_policy::{Approval, Scope};
use corral_secrets::{Provider, SecretStore};
use corral_store::Store;
use corral_tools::{Answer, CheckpointDesk};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// The skeleton's per-run hard cap: $0.25 (docs/03, J0).
pub const RUN_CAP_MICRO_USD: u64 = 250_000;
/// Where a run asks on its card before spending more: $0.10 (TP-BUD-02). An agent that can't
/// ask (one whose folder couldn't be made, so it has no tools) has each request held under it
/// instead.
pub const SOFT_LIMIT_MICRO_USD: u64 = 100_000;
/// The model Grok's drop uses: the catalog's, which the nightly live check watches (EV-PROV-03).
pub const GROK_MODEL: &str = corral_adapters::catalog::GROK_MODEL;
/// The model Claude's drop uses (SK-7): the catalog's, which the nightly live check watches.
pub const CLAUDE_MODEL: &str = corral_adapters::catalog::CLAUDE_MODEL;

/// The providers with a key in the Keychain. A key that can't be read counts as missing (the
/// card would fail on it anyway) and is logged.
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

/// The linked apps the user chose, and where the choice is kept.
struct Linked {
    store: Option<LinkedStore>,
    settings: Mutex<LinkedSettings>,
}

/// What `setup` keeps alive for the life of the app. Cheap to clone: the intent callback holds
/// one too.
#[derive(Clone)]
pub struct Corral {
    /// Keeps the corral in step with the log.
    pub feed: Arc<CorralFeed>,
    /// Turns clicks into runs, once the event store and the provider client are ready.
    pub controls: Option<Arc<CorralControls>>,
    /// Logos, when the app has a data folder.
    pub logos: Option<LogoStore>,
    linked: Arc<Linked>,
    keys: Arc<dyn SecretStore>,
    /// Where agents' tool calls and spend checkpoints wait for the user's answer from the card.
    desk: Arc<CheckpointDesk>,
    /// The Touch ID prompt for a critical checkpoint, if one is up (TP-CHK-17).
    touch_id: Arc<Mutex<TouchIdDesk>>,
}

impl Corral {
    /// "Approve with Touch ID" on `agent`'s card: if the card shows that critical checkpoint now,
    /// Swift's prompt asks macOS (Touch ID, or the login password when Touch ID can't be used).
    /// The prompt is shown after Swift's callback returns, never inside it.
    fn ask_touch_id(&self, agent: &AgentId, checkpoint: &CheckpointId) {
        let Some((card, name)) = self.feed.card_and_name(agent) else {
            eprintln!("corral: {agent} has no card, so Touch ID wasn't asked");
            return;
        };
        let request = {
            let Ok(mut desk) = self.touch_id.lock() else {
                eprintln!("corral: the Touch ID desk is poisoned, so Touch ID wasn't asked");
                return;
            };
            match desk.begin(&card, checkpoint, &name) {
                Ok(request) => request,
                Err(e) => {
                    eprintln!("corral: no Touch ID prompt for {agent}'s card: {e}");
                    return;
                }
            }
        };
        let touch_id = Arc::clone(&self.touch_id);
        let posted = self.feed.app.run_on_main_thread(move || {
            if let Err(e) = native::authenticate(&request) {
                eprintln!("corral: couldn't show the Touch ID prompt: {e}");
                abandon(&touch_id);
            }
        });
        if let Err(e) = posted {
            eprintln!("corral: couldn't reach the main thread for Touch ID: {e}");
            abandon(&self.touch_id);
        }
    }

    /// A prompt's result. Only `ok` for the request that's up approves, with Touch ID, once; the
    /// desk and the policy check it like any answer.
    fn touch_id_result(&self, request: &str, outcome: AuthOutcome) {
        let finished = {
            let Ok(mut desk) = self.touch_id.lock() else {
                eprintln!("corral: the Touch ID desk is poisoned, so the result was dropped");
                return;
            };
            desk.finish(request, outcome)
        };
        match finished {
            Finished::Approve { agent, checkpoint } => self.answer(
                &agent,
                &checkpoint,
                Answer::Approve {
                    how: Approval::TouchId,
                    scope: Scope::Once,
                },
            ),
            Finished::NotApproved { agent, outcome, .. } => eprintln!(
                "corral: Touch ID didn't approve {agent}'s checkpoint ({outcome:?}); it's still waiting"
            ),
            Finished::Stale => eprintln!("corral: a Touch ID result for an old prompt was ignored"),
        }
    }

    /// Answers the checkpoint on `agent`'s card. Only the checkpoint that card shows now can be
    /// answered from it (TP-HOVER-06); the desk takes each one once, and the policy checks the
    /// gesture. A refusal is logged; the card follows the log either way.
    fn answer(&self, agent: &AgentId, checkpoint: &CheckpointId, answer: Answer) {
        if !self.feed.shows_checkpoint(agent, checkpoint) {
            eprintln!(
                "corral: {agent}'s card doesn't show that checkpoint now, so the answer was dropped"
            );
            return;
        }
        if let Err(e) = self.desk.answer(checkpoint, answer) {
            eprintln!("corral: the answer on {agent}'s card wasn't taken: {e}");
        }
    }

    /// Approves the edited call from `agent`'s edit sheet (TP-CHK-15): the core merges the
    /// changed fields into the agent's arguments and checks them, then the desk and the policy
    /// treat it like any answer. A refusal is logged; the card follows the log either way.
    fn edit(
        &self,
        agent: &AgentId,
        checkpoint: &CheckpointId,
        changes: &BTreeMap<String, String>,
        how: Approval,
    ) {
        match self.feed.edited_args(agent, checkpoint, changes) {
            Ok(args) => self.answer(agent, checkpoint, Answer::Edit { how, args }),
            Err(e) => eprintln!("corral: the edit on {agent}'s card wasn't taken: {e}"),
        }
    }

    /// Brings the corral in line with the keys in `store` and the linked apps: a newly connected
    /// provider's agent drops in, and a removed one leaves (TP-VIEW-06). Called after a key or a
    /// linked app changes.
    pub fn refresh(&self, store: &dyn SecretStore) {
        let connected = connected(store);
        let linked = self.linked_settings();
        if let Some(controls) = &self.controls {
            // Only the agents Corral runs take tasks; a linked drop never does (ADR-005).
            controls.set_agents(agents_for(&connected).into_iter().map(|c| c.id));
        }
        self.feed
            .set_agents(corral_agents(&connected, &linked), linked);
        self.show_logos(&connected, &linked);
    }

    fn linked_settings(&self) -> LinkedSettings {
        match self.linked.settings.lock() {
            Ok(settings) => *settings,
            Err(_) => {
                eprintln!("corral: the linked apps are poisoned; showing none");
                LinkedSettings::default()
            }
        }
    }

    /// Shows or hides a linked app from the right-click menu, saves the choice and updates the
    /// corral. A choice that can't be saved still applies until Corral quits, and is logged.
    pub fn change_linked(&self, change: LinkChange) {
        {
            let Ok(mut settings) = self.linked.settings.lock() else {
                eprintln!("corral: the linked apps are poisoned; {change:?} was dropped");
                return;
            };
            let mut next = *settings;
            if !next.apply(change) {
                return;
            }
            if let Some(store) = &self.linked.store
                && let Err(e) = store.save(&next)
            {
                eprintln!("corral: {e}; the change lasts until Corral quits");
            }
            *settings = next;
        }
        self.refresh(self.keys.as_ref());
    }

    /// Opens a linked app's own site (ADR-005): a fixed URL, never one from Swift.
    pub fn open_linked(&self, app: LinkedApp) {
        let url = app.open_url();
        // The task reports its own outcome, so its handle is dropped.
        tauri::async_runtime::spawn_blocking(move || {
            match std::process::Command::new("/usr/bin/open")
                .arg(url)
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => eprintln!("corral: opening {url} ended with {status}"),
                Err(e) => eprintln!("corral: couldn't open {url}: {e}"),
            }
        });
    }

    /// Shows each drop's logo: a saved one at once, a missing one once it's fetched, off the
    /// main thread. A logo that can't be had is logged; the drop keeps its monogram.
    fn show_logos(&self, connected: &[Provider], linked: &LinkedSettings) {
        let Some(logos) = self.logos.clone() else {
            return;
        };
        let providers = connected
            .iter()
            .map(|p| (LogoId::Provider(*p), agent_for(*p).id));
        let apps = linked
            .shown()
            .into_iter()
            .filter_map(|app| linked.logo(app).map(|id| (id, app.agent_card().id)));
        let wanted: Vec<(LogoId, AgentId)> = providers.chain(apps).collect();
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
    let corral_slot: Arc<OnceLock<Corral>> = Arc::new(OnceLock::new());
    let slot = Arc::clone(&corral_slot);
    native::start(move |intent| on_intent(&slot, intent))?;

    let data_dir = match app.path().app_data_dir() {
        Ok(dir) => Some(dir),
        Err(e) => {
            eprintln!("corral: no app data folder, so no logos and no saved linked apps: {e}");
            None
        }
    };
    let logos = data_dir.as_ref().map(|d| LogoStore::new(d.join("logos")));
    let linked_store = data_dir
        .as_ref()
        .map(|d| LinkedStore::new(d.join("linked-apps.json")));
    let linked = match linked_store.as_ref().map(LinkedStore::load) {
        None => LinkedSettings::default(),
        Some(Ok(settings)) => settings,
        Some(Err(e)) => {
            eprintln!("corral: {e}; no linked apps this time");
            LinkedSettings::default()
        }
    };

    let keys = app.state::<Keys>().0.clone();
    let connected = connected(keys.as_ref());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    // The usage order, as saved; the log's deploys seed it once the store is open.
    let mut order = CorralOrder::open(data_dir.as_ref().map(|d| d.join("corral-order.json")));
    if let Some(problem) = order.problem() {
        eprintln!("corral: {problem}");
    }
    let all_cards = corral_agents(&connected, &linked);
    let placed = order.place(all_cards.clone(), clock.now_ms());
    if placed.choose_shown {
        eprintln!("corral: the corral holds up to 8 agents; a new one is hidden until you choose");
    }
    save_order(&mut order);
    let feed = Arc::new(CorralFeed {
        views: Mutex::new(Views {
            corral: CorralTracker::new(placed.cards),
            order,
            all_cards,
            cards: RunCards::default(),
            logos: HashMap::new(),
            linked,
        }),
        app: app.clone(),
        clock: Arc::clone(&clock),
    });
    feed.publish_now()?;
    feed.start_ticking();

    // Only the agents Corral runs take tasks; linked drops never do (ADR-005).
    let runnable: Vec<AgentId> = agents_for(&connected).into_iter().map(|c| c.id).collect();
    let desk = Arc::new(CheckpointDesk::default());
    let controls = match supervisor(app, &feed, clock, &desk) {
        Ok(supervisor) => Some(Arc::new(CorralControls::new(
            supervisor,
            runnable,
            id_source(),
        ))),
        Err(reason) => {
            eprintln!("corral: tasks are off: {reason}");
            None
        }
    };
    let corral = Corral {
        feed,
        controls,
        logos,
        linked: Arc::new(Linked {
            store: linked_store,
            settings: Mutex::new(linked),
        }),
        keys,
        desk,
        touch_id: Arc::new(Mutex::new(TouchIdDesk::default())),
    };
    // The slot is empty: this is its only writer, and `start` runs once.
    if corral_slot.set(corral.clone()).is_err() {
        eprintln!("corral: the corral was already set; keeping the first");
    }
    corral.show_logos(&connected, &linked);
    Ok(corral)
}

/// The supervisor: each agent's runs go to its provider (Grok and Claude have clients so far),
/// the event store, the $0.25 cap and the $0.10 soft limit. Tool checkpoints wait on `desk`.
fn supervisor(
    app: &AppHandle,
    feed: &Arc<CorralFeed>,
    clock: Arc<dyn Clock>,
    desk: &Arc<CheckpointDesk>,
) -> Result<Supervisor, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data folder: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let store = Arc::new(
        Store::open(&dir.join("corral.db"))
            .map_err(|e| format!("couldn't open the event store: {e}"))?,
    );
    // The main window's trace viewer reads the same store.
    app.state::<Arc<Traces>>().attach(Arc::clone(&store));
    if let Some(banner) = store.integrity().banner() {
        eprintln!("corral: {banner}");
    }
    match store.fail_orphaned_runs(clock.now_ms()) {
        Ok(failed) if failed.is_empty() => {}
        Ok(failed) => eprintln!("corral: marked {} unfinished runs as failed", failed.len()),
        Err(e) => eprintln!("corral: couldn't close out unfinished runs: {e}"),
    }
    // The last 90 days of deploys, so the corral opens most used first (TP-USAGE-06). Without
    // them the saved order stands.
    match store.deploys_since(clock.now_ms().saturating_sub(WINDOW_MS)) {
        Ok(deploys) => feed.seed_usage(deploys),
        Err(e) => {
            eprintln!("corral: couldn't read recent deploys, so the order stays as saved: {e}")
        }
    }
    // This month's spend, so relaunching doesn't reset the monthly caps (TP-BUD-07). Failing to
    // read it fails closed: counting from nothing could spend past a cap.
    let ledger = Arc::new(SpendLedger::default());
    let spends = store
        .spend_since(clock.now_ms().saturating_sub(SPEND_LOOKBACK_MS))
        .map_err(|e| format!("couldn't read this month's spend from the event store: {e}"))?;
    for spend in spends {
        ledger.seed(&spend.agent, spend.ts_ms, spend.cost_micro_usd);
    }
    let transport = ReqwestTransport::new().map_err(|e| format!("no HTTPS client: {e}"))?;
    let keys = app.state::<Keys>().0.clone();
    let transport: Arc<dyn Transport> = Arc::new(transport);
    // Without its folder, an agent still answers, with no tools.
    let grok: Arc<dyn RunDriver> = match agent_file_tools(app, desk, Provider::Xai) {
        Ok(tools) => Arc::new(ResponsesAgent::new(
            transport.clone(),
            keys.clone(),
            ResponsesConfig::xai(GROK_MODEL),
            tools,
        )),
        Err(reason) => {
            eprintln!("corral: Grok has no tools: {reason}");
            Arc::new(ResponsesDriver::new(
                transport.clone(),
                keys.clone(),
                ResponsesConfig::xai(GROK_MODEL),
            ))
        }
    };
    let claude: Arc<dyn RunDriver> = match agent_file_tools(app, desk, Provider::Anthropic) {
        Ok(tools) => claude_agent(app, transport, keys, tools),
        Err(reason) => {
            eprintln!("corral: Claude has no tools: {reason}");
            Arc::new(MessagesDriver::new(
                transport,
                keys,
                MessagesConfig::anthropic(CLAUDE_MODEL),
            ))
        }
    };
    let mut drivers: HashMap<AgentId, Arc<dyn RunDriver>> = HashMap::new();
    drivers.insert(agent_for(Provider::Xai).id, grok);
    drivers.insert(agent_for(Provider::Anthropic).id, claude);
    let driver = AgentRouter::new(drivers);
    let built = Supervisor::with_ledger(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: RUN_CAP_MICRO_USD,
        },
        ledger,
        Arc::new(driver),
        feed.watch(store),
        clock,
    );
    built.set_soft_limit(Some(SOFT_LIMIT_MICRO_USD));
    Ok(built)
}

/// Claude with its file tools, and with web search when the user turned it on in the main window
/// (TP-PROV-13): the switch is read as each run starts. Without a way to allow it, Claude runs
/// without search.
fn claude_agent(
    app: &AppHandle,
    transport: Arc<dyn Transport>,
    keys: Arc<dyn SecretStore>,
    tools: ToolSetup,
) -> Arc<dyn RunDriver> {
    let plain: Arc<dyn RunDriver> = Arc::new(MessagesAgent::new(
        transport.clone(),
        keys.clone(),
        MessagesConfig::anthropic(CLAUDE_MODEL),
        tools.clone(),
    ));
    let searching_tools = match allowing_provider_tools(tools) {
        Ok(searching_tools) => searching_tools,
        Err(reason) => {
            eprintln!("corral: Claude can't search the web: {reason}");
            return plain;
        }
    };
    let searching: Arc<dyn RunDriver> = Arc::new(MessagesAgent::new(
        transport,
        keys,
        MessagesConfig {
            web_search: true,
            ..MessagesConfig::anthropic(CLAUDE_MODEL)
        },
        searching_tools,
    ));
    let choices = Arc::clone(app.state::<Arc<ProviderTools>>().inner());
    let claude = agent_for(Provider::Anthropic).id;
    Arc::new(Switched::new(
        searching,
        plain,
        Arc::new(move || choices.is_on(&claude, ProviderTool::WebSearch)),
    ))
}

/// The file tools over `~/Corral/<agent name>` for `provider`'s agent, with checkpoints on
/// `desk`.
fn agent_file_tools(
    app: &AppHandle,
    desk: &Arc<CheckpointDesk>,
    provider: Provider,
) -> Result<ToolSetup, String> {
    let home = app
        .path()
        .home_dir()
        .map_err(|e| format!("no home folder: {e}"))?;
    let name = agent_for(provider).name;
    file_tools(&agent_folder(&home, &name), Arc::clone(desk))
}

/// How far back launch reads spend: past the start of this month in any time zone.
const SPEND_LOOKBACK_MS: u64 = 32 * 24 * 60 * 60 * 1_000;

/// Unique run and session ID suffixes: the start time plus a counter.
fn id_source() -> Arc<dyn Fn() -> String + Send + Sync> {
    let started = SystemClock.now_ms();
    let next = AtomicU64::new(1);
    Arc::new(move || format!("{started:x}-{}", next.fetch_add(1, Ordering::Relaxed)))
}

struct Views {
    corral: CorralTracker,
    /// The corral's usage order and shown set (TP-USAGE-06..09).
    order: CorralOrder,
    /// Every agent, before the order picks and sorts the shown ones.
    all_cards: Vec<AgentCard>,
    cards: RunCards,
    /// Logo paths by agent.
    logos: HashMap<AgentId, String>,
    /// Which linked apps show, for the right-click menu.
    linked: LinkedSettings,
}

/// Keeps the corral in step with the event log.
pub struct CorralFeed {
    views: Mutex<Views>,
    app: AppHandle,
    clock: Arc<dyn Clock>,
}

impl CorralFeed {
    /// Whether `agent`'s card shows `checkpoint` now (the core decides, from the log).
    fn shows_checkpoint(&self, agent: &AgentId, checkpoint: &CheckpointId) -> bool {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no checkpoint can be answered");
            return false;
        };
        views.cards.shows_checkpoint(agent, checkpoint)
    }

    /// `agent`'s card and the name its drop shows, for a Touch ID prompt.
    fn card_and_name(&self, agent: &AgentId) -> Option<(RunCard, String)> {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no card can be read");
            return None;
        };
        let card = views.cards.card(agent, self.clock.now_ms())?;
        let name = views
            .corral
            .view()
            .drops
            .into_iter()
            .find(|drop| drop.agent.id == *agent)
            .map_or_else(|| agent.to_string(), |drop| drop.agent.name);
        Some((card, name))
    }

    /// The agent's arguments with the card's changes merged in (the core decides, from the log).
    fn edited_args(
        &self,
        agent: &AgentId,
        checkpoint: &CheckpointId,
        changes: &BTreeMap<String, String>,
    ) -> Result<Value, EditError> {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no edit can be taken");
            return Err(EditError::NotShown);
        };
        views.cards.edited_args(agent, checkpoint, changes)
    }

    /// Wraps `log` so every event it accepts updates the corral. Give the result to the
    /// supervisor as its event log.
    pub fn watch(self: &Arc<Self>, log: Arc<dyn EventLog>) -> Arc<dyn EventLog> {
        let feed = Arc::clone(self);
        Arc::new(WatchedLog::new(log, move |event: &Event| {
            feed.observe(event)
        }))
    }

    /// Replaces the corral's agents and, if the corral changed, sends the new roster. The usage
    /// order picks and sorts the shown ones (TP-USAGE-07, 08).
    fn set_agents(&self, cards: Vec<AgentCard>, linked: LinkedSettings) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        views.all_cards = cards.clone();
        let placed = views.order.place(cards, self.clock.now_ms());
        if placed.choose_shown {
            eprintln!(
                "corral: the corral holds up to 8 agents; a new one is hidden until you choose"
            );
        }
        save_order(&mut views.order);
        let drops_changed = views.corral.set_agents(placed.cards);
        let links_changed = views.linked != linked;
        views.linked = linked;
        if drops_changed || links_changed {
            self.send(self.snapshot(&views));
        }
    }

    /// At launch, once the store is open: the last 90 days of deploys, and the order they give,
    /// published before anything can be under the pointer (TP-USAGE-06).
    fn seed_usage(&self, deploys: Vec<Deploy>) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        let now = self.clock.now_ms();
        if !views.order.seed(deploys, now) {
            return;
        }
        save_order(&mut views.order);
        let cards = views.all_cards.clone();
        let placed = views.order.place(cards, now);
        if views.corral.set_agents(placed.cards) {
            self.send(self.snapshot(&views));
        }
    }

    /// Swift's report that the corral turned idle or busy (TP-USAGE-10). Idle with an order
    /// waiting and nothing waiting on the user, the new order is sent.
    fn corral_idle(&self, idle: bool) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        views.order.set_corral_idle(idle);
        if settle(&mut views, self.clock.now_ms()) {
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
            .with_linked(&views.linked)
    }

    /// Updates the views and, when a drop or a card changed, sends the new roster to Swift. The
    /// roster is queued for the main thread while the views are still locked, so updates reach
    /// Swift in the order they happened.
    fn observe(&self, event: &Event) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        // Deploys count toward the usage order at once; a new order waits for the corral to be
        // idle (TP-USAGE-07).
        if let EventBody::RunQueued(queued) = &event.body {
            views.order.deployed(queued.agent_id.clone(), event.ts);
        }
        if let EventBody::RunCompleted(_) | EventBody::RunFailed(_) | EventBody::RunCancelled(_) =
            &event.body
        {
            views.order.run_ended(event.ts);
        }
        // The supervisor already validated the event; a refusal here means the views diverged.
        let drop_changed = views.corral.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the corral view refused {:?}: {e}", event.kind());
            false
        });
        let card_changed = views.cards.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the run cards refused {:?}: {e}", event.kind());
            false
        });
        // A run ending or a checkpoint answered can leave the corral idle with an order waiting.
        let order_changed = match &event.body {
            EventBody::RunCompleted(_)
            | EventBody::RunFailed(_)
            | EventBody::RunCancelled(_)
            | EventBody::CheckpointResolved(_) => settle(&mut views, self.clock.now_ms()),
            EventBody::RunQueued(_)
            | EventBody::RunStarted(_)
            | EventBody::PlanUpdated(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::ModelResponse(_)
            | EventBody::ToolRequested(_)
            | EventBody::PolicyDecided(_)
            | EventBody::CheckpointOpened(_)
            | EventBody::ToolStarted(_)
            | EventBody::ToolResult(_)
            | EventBody::UsageReported(_)
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::SessionClosed(_) => false,
        };
        if drop_changed || card_changed || order_changed {
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
/// thread; so does Stop all. Approve and Deny on a card go to the checkpoint desk. Linked apps are shown, hidden and opened here, with fixed URLs
/// (ADR-005). The rest
/// are reported until their milestones.
fn on_intent(slot: &OnceLock<Corral>, result: Result<Intent, BridgeError>) {
    let intent = match result {
        Ok(intent) => intent,
        Err(e) => {
            eprintln!("corral: refused an intent from CorralNative: {e}");
            return;
        }
    };
    let Some(corral) = slot.get().cloned() else {
        eprintln!("corral: still starting, so {intent:?} was dropped");
        return;
    };
    // Each task's handle is dropped on purpose: the task reports its own outcome. Linked
    // changes read the Keychain, so they also run off the main thread.
    match intent {
        Intent::ShowLinked { app, look } => {
            tauri::async_runtime::spawn_blocking(move || {
                corral.change_linked(LinkChange::Show { app, look });
            });
        }
        Intent::HideLinked(app) => {
            tauri::async_runtime::spawn_blocking(move || {
                corral.change_linked(LinkChange::Hide(app));
            });
        }
        Intent::OpenLinked(app) => corral.open_linked(app),
        Intent::CorralIdle(idle) => corral.feed.corral_idle(idle),
        Intent::StopAll => {
            let Some(controls) = corral.controls.clone() else {
                eprintln!("corral: tasks are off, so Stop all had nothing to stop");
                return;
            };
            tauri::async_runtime::spawn(async move {
                let report = controls.stop_all().await;
                eprintln!(
                    "corral: Stop all stopped {} runs ({} had to be dropped)",
                    report.stopped, report.forced
                );
            });
        }
        Intent::DeployTask { agent, prompt } => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            tauri::async_runtime::spawn(async move {
                report(
                    "deploy",
                    &agent,
                    controls.deploy(&agent, &prompt).map(|_| ()),
                );
            });
        }
        Intent::Stop(agent) => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            tauri::async_runtime::spawn(async move {
                report("stop", &agent, controls.stop(&agent).await);
            });
        }
        Intent::SendBack(agent) => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            tauri::async_runtime::spawn(async move {
                report("send back", &agent, controls.send_back(&agent));
            });
        }
        Intent::Retry(agent) => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            tauri::async_runtime::spawn(async move {
                report("retry", &agent, controls.retry(&agent).map(|_| ()));
            });
        }
        Intent::ApproveCheckpoint {
            agent,
            checkpoint,
            scope,
            how,
        } => corral.answer(&agent, &checkpoint, Answer::Approve { how, scope }),
        Intent::DenyCheckpoint {
            agent,
            checkpoint,
            note,
        } => corral.answer(&agent, &checkpoint, Answer::Deny { note }),
        Intent::EditCheckpoint {
            agent,
            checkpoint,
            changes,
            how,
        } => corral.edit(&agent, &checkpoint, &changes, how),
        Intent::ApproveWithTouchId { agent, checkpoint } => {
            corral.ask_touch_id(&agent, &checkpoint);
        }
        Intent::AuthResult { request, outcome } => corral.touch_id_result(&request, outcome),
        Intent::OpenAgent(_)
        | Intent::Deploy(_)
        | Intent::SummonPromptBar
        | Intent::NotificationAction { .. }
        | Intent::NotificationFailed { .. } => {
            eprintln!("corral: intent {intent:?} (handled in a later milestone)");
        }
    }
}

/// Publishes a waiting usage order if the corral is idle and no drop waits on the user
/// (TP-USAGE-12), and re-places the drops. Returns whether the corral changed.
fn settle(views: &mut Views, now_ms: u64) -> bool {
    let waiting = views
        .corral
        .view()
        .drops
        .iter()
        .any(|drop| drop.state == DropState::NeedsYou);
    if !views.order.settle(waiting) {
        return false;
    }
    save_order(&mut views.order);
    let cards = views.all_cards.clone();
    let placed = views.order.place(cards, now_ms);
    views.corral.set_agents(placed.cards)
}

/// Saves the corral's order. A failure is logged; the corral keeps working unsaved.
fn save_order(order: &mut CorralOrder) {
    if let Err(e) = order.save() {
        eprintln!("corral: {e}");
    }
}

/// Drops the prompt that's up: Swift couldn't show it.
fn abandon(touch_id: &Mutex<TouchIdDesk>) {
    match touch_id.lock() {
        Ok(mut desk) => desk.abandon(),
        Err(_) => eprintln!("corral: the Touch ID desk is poisoned; its prompt stays marked up"),
    }
}

/// The controls, or a logged reason why a task click went nowhere.
fn tasks(corral: &Corral, agent: &AgentId) -> Option<Arc<CorralControls>> {
    let controls = corral.controls.clone();
    if controls.is_none() {
        eprintln!("corral: tasks are off, so a click on {agent} was dropped (see the startup log)");
    }
    controls
}

/// Logs a refused click. The hover card shows these from SK-6b.
fn report(what: &str, agent: &AgentId, outcome: Result<(), ControlError>) {
    if let Err(e) = outcome {
        eprintln!("corral: {what} for {agent} refused: {e}");
    }
}
