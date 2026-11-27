//! Connects the core to the corral (CorralNative, ADR-001).
//!
//! The core decides everything the corral shows and does (AGENTS.md rule 3); this module only
//! relays:
//! - **Out to Swift:** [`CorralFeed::watch`] wraps the event log, so every event the log accepts
//!   updates what the corral shows ([`CorralState`]: the drops, their order and the run cards).
//!   Each change, and a tick once a second while a card's clock is moving, sends the roster with
//!   its cards to Swift on the main thread.
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

use crate::agent_editor::AgentEditor;
use crate::agent_names::AgentNames;
use crate::agent_policies::PolicyEditor;
use crate::agent_settings::{AgentSettings, messages_config, responses_config, seed_caps};
use crate::agent_tools::{allowing_provider_tools, file_tools_under};
use crate::bridge::{
    AuthOutcome, BridgeError, Intent, NotificationRequest, PromptBarSnapshot, RosterSnapshot,
};
use crate::chatgpt_signin::{ChatGptSignIn, plan_signed_in, with_plan};
use crate::corral_order::{CorralOrder, CorralSettingsView};
use crate::corral_settings::{CorralControl, CorralSettings};
use crate::corral_state::{CorralState, save_order};
use crate::keys::Keys;
use crate::linked::LinkedStore;
use crate::logos::{LogoStore, fetch_missing};
use crate::native;
use crate::prompt_bar_feed;
use crate::provider_tools::ProviderTools;
use crate::touch_id::{Finished, TouchIdDesk};
use crate::traces::Traces;
use corral_adapters::agent::{MessagesAgent, ProviderTool, ResponsesAgent, ToolSetup};
use corral_adapters::catalog::{
    AgentRouter, ModelChoice, Rebuilt, Switched, agent_for, agents_for, corral_agents, models,
};
use corral_adapters::chatgpt::{ChatGptPlan, PlanAgent, PlanBuild};
use corral_adapters::linked::{LinkChange, LinkedApp, LinkedSettings};
use corral_adapters::logos::{LogoId, ReqwestLogoFetcher};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_adapters::stream::KeySource;
use corral_adapters::transport::{ReqwestTransport, Transport};
use corral_core::budget::{BudgetConfig, SpendLedger};
use corral_core::checkpoint_note::{NoteAnswer, NoteClick, NoteTrouble, Notice};
use corral_core::clock::{Clock, SystemClock};
use corral_core::controls::{ControlError, CorralControls};
use corral_core::corral_view::AgentCard;
use corral_core::event::Event;
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::prompt_bar;
use corral_core::run_card::{EditError, RunCard};
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

/// The providers whose agents run: those with a key in the Keychain, and OpenAI's while the
/// ChatGPT plan is signed in (TP-SUB-07). A key that can't be read counts as missing (the card
/// would fail on it anyway) and is logged.
fn connected(store: &dyn SecretStore, plan: &ChatGptPlan) -> Vec<Provider> {
    let keyed = Provider::ALL
        .into_iter()
        .filter(|p| match corral_secrets::status(store, *p) {
            Ok(status) => status.configured,
            Err(e) => {
                eprintln!("corral: couldn't read the {} key: {e}", p.display_name());
                false
            }
        })
        .collect();
    with_plan(keyed, plan_signed_in(plan))
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
    /// The agents' names, for their cards (TP-ANM-03).
    names: AgentNames,
    /// The ChatGPT plan GPT runs on (ADR-017).
    plan: Arc<ChatGptPlan>,
}

impl Corral {
    /// "Approve with Touch ID" on `agent`'s card: if the card shows that critical checkpoint now,
    /// Swift's prompt asks macOS (Touch ID, or the login password when Touch ID can't be used).
    /// The prompt is shown after Swift's callback returns, never inside it.
    fn ask_touch_id(&self, agent: &AgentId, checkpoint: &CheckpointId) {
        let Some((card, name)) = self.feed.card_and_name(agent, checkpoint) else {
            eprintln!(
                "corral: that checkpoint doesn't wait on {agent}'s card, so Touch ID wasn't asked"
            );
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

    /// Answers a checkpoint on `agent`'s card. Only one waiting on that card can be answered
    /// from it, the one the user stepped to (TP-HOVER-06, TP-CHK-19); the desk takes each one
    /// once, and the policy checks the gesture. A refusal is logged; the card follows the log
    /// either way.
    fn answer(&self, agent: &AgentId, checkpoint: &CheckpointId, answer: Answer) {
        if !self.feed.waits_on(agent, checkpoint) {
            eprintln!(
                "corral: that checkpoint doesn't wait on {agent}'s card, so the answer was dropped"
            );
            return;
        }
        if let Err(e) = self.desk.answer(checkpoint, answer) {
            eprintln!("corral: the answer on {agent}'s card wasn't taken: {e}");
        }
    }

    /// A click on a checkpoint notification. The core checks its nonce and the risk; an approve or
    /// deny is then answered from that agent's card like any other (TP-CHK-23). Open is drawn by
    /// Swift, which opens the card the core already sent.
    fn note_click(&self, checkpoint: &CheckpointId, nonce: &str, click: NoteClick) {
        match self.feed.note_click(checkpoint, nonce, click) {
            NoteAnswer::Approve { agent, checkpoint } => self.answer(
                &agent,
                &checkpoint,
                Answer::Approve {
                    how: Approval::Click,
                    scope: Scope::Once,
                },
            ),
            NoteAnswer::Deny { agent, checkpoint } => {
                self.answer(&agent, &checkpoint, Answer::Deny { note: None });
            }
            NoteAnswer::Open { agent, .. } => {
                eprintln!("corral: {agent}'s card opened from a notification");
            }
            NoteAnswer::Refused { agent, .. } => eprintln!(
                "corral: a notification can't approve {agent}'s checkpoint; it waits on the card"
            ),
            NoteAnswer::Dismissed => {}
            NoteAnswer::Stale => eprintln!("corral: a click on an old notification was ignored"),
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
        let connected = connected(store, &self.plan);
        let linked = self.linked_settings();
        if let Some(controls) = &self.controls {
            // Only the agents Corral runs take tasks; a linked drop never does (ADR-005).
            controls.set_agents(agents_for(&connected).into_iter().map(|c| c.id));
        }
        self.feed.set_agents(
            self.names.rename(corral_agents(&connected, &linked)),
            linked,
        );
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
    let plan = Arc::clone(app.state::<Arc<ChatGptSignIn>>().plan());
    let connected = connected(keys.as_ref(), &plan);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    // The usage order, as saved; the log's deploys seed it once the store is open.
    let order = CorralOrder::open(data_dir.as_ref().map(|d| d.join("corral-order.json")));
    if let Some(problem) = order.problem() {
        eprintln!("corral: {problem}");
    }
    let names = app.state::<AgentNames>().inner().clone();
    let all_cards = names.rename(corral_agents(&connected, &linked));
    let feed = Arc::new(CorralFeed {
        views: Mutex::new(CorralState::open(order, all_cards, linked, clock.now_ms())),
        app: app.clone(),
        clock: Arc::clone(&clock),
    });
    feed.publish_now()?;
    // The main window's Corral section changes this corral.
    app.state::<Arc<CorralSettings>>()
        .attach(Arc::clone(&feed) as Arc<dyn CorralControl>);
    feed.start_ticking();

    // Only the agents Corral runs take tasks; linked drops never do (ADR-005).
    let runnable: Vec<AgentId> = agents_for(&connected).into_iter().map(|c| c.id).collect();
    let desk = Arc::new(CheckpointDesk::default());
    let controls = match supervisor(app, &feed, clock, &desk) {
        Ok(supervisor) => {
            // Cards offer Pause only for agents whose driver stops at safe points (TP-PAUSE-01).
            feed.set_pausable(
                Provider::ALL
                    .iter()
                    .map(|provider| agent_for(*provider).id)
                    .filter(|agent| supervisor.can_pause(agent)),
            );
            Some(Arc::new(CorralControls::new(
                supervisor,
                runnable,
                id_source(),
            )))
        }
        Err(reason) => {
            eprintln!("corral: tasks are off: {reason}");
            None
        }
    };
    let corral = Corral {
        names,
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
        plan,
    };
    // The slot is empty: this is its only writer, and `start` runs once.
    if corral_slot.set(corral.clone()).is_err() {
        eprintln!("corral: the corral was already set; keeping the first");
    }
    // A rename in the agent editor shows on the agent's card at once (TP-AED-06).
    let told = corral.clone();
    app.state::<Arc<AgentEditor>>()
        .on_change(Box::new(move || told.refresh(told.keys.as_ref())));
    corral.show_logos(&connected, &linked);
    Ok(corral)
}

/// The supervisor: each agent's runs go to its provider (Grok and Claude on their keys, GPT on
/// the ChatGPT plan), the event store, the $0.25 cap and the $0.10 soft limit. Tool checkpoints wait on `desk`.
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
    // Each agent's own monthly cap (TP-AED-04). Settings that can't be used leave it at $0.
    let catalog: Vec<AgentId> = Provider::ALL
        .into_iter()
        .map(|provider| agent_for(provider).id)
        .collect();
    for problem in seed_caps(&ledger, app.state::<AgentSettings>().inner(), &catalog) {
        eprintln!("corral: {problem}");
    }
    // A cap saved in the agent editor goes into this ledger, from the agent's next request.
    let agent_editor = Arc::clone(app.state::<Arc<AgentEditor>>().inner());
    agent_editor.attach(Arc::clone(&ledger), Arc::clone(&clock));
    let transport = ReqwestTransport::new().map_err(|e| format!("no HTTPS client: {e}"))?;
    let keys = app.state::<Keys>().0.clone();
    let transport: Arc<dyn Transport> = Arc::new(transport);
    // Each agent is built again for its next run after its policy or its settings are saved
    // (TP-PED-07, TP-AED-06).
    let editor = app.state::<PolicyEditor>().inner().clone();
    let grok = rebuilt_on_save(&editor, &agent_editor, Provider::Xai, {
        let (app, desk, transport, keys) = (
            app.clone(),
            Arc::clone(desk),
            transport.clone(),
            keys.clone(),
        );
        move || grok_agent(&app, &desk, transport.clone(), keys.clone())
    });
    let claude = rebuilt_on_save(&editor, &agent_editor, Provider::Anthropic, {
        let (app, desk, transport) = (app.clone(), Arc::clone(desk), transport.clone());
        move || claude_with_tools(&app, &desk, transport.clone(), keys.clone())
    });
    let gpt = rebuilt_on_save(&editor, &agent_editor, Provider::OpenAi, {
        let (app, desk) = (app.clone(), Arc::clone(desk));
        let plan = Arc::clone(app.state::<Arc<ChatGptSignIn>>().plan());
        move || gpt_agent(&app, &desk, transport.clone(), Arc::clone(&plan))
    });
    let mut drivers: HashMap<AgentId, Arc<dyn RunDriver>> = HashMap::new();
    drivers.insert(agent_for(Provider::Xai).id, grok);
    drivers.insert(agent_for(Provider::Anthropic).id, claude);
    drivers.insert(agent_for(Provider::OpenAi).id, gpt);
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

/// `provider`'s agent from `build`, built again for the run after its policy or its settings
/// are saved. Each count only grows, so their sum changes whenever either does.
fn rebuilt_on_save(
    editor: &PolicyEditor,
    agent_editor: &Arc<AgentEditor>,
    provider: Provider,
    build: impl Fn() -> Arc<dyn RunDriver> + Send + Sync + 'static,
) -> Arc<dyn RunDriver> {
    let editor = editor.clone();
    let agent_editor = Arc::clone(agent_editor);
    let agent = agent_for(provider).id;
    Arc::new(Rebuilt::new(
        Arc::new(move || {
            editor
                .version(&agent)
                .saturating_add(agent_editor.version(&agent))
        }),
        Arc::new(build),
    ))
}

/// Grok with its file tools. Without its folder or a usable policy, it still answers, with no
/// tools.
fn grok_agent(
    app: &AppHandle,
    desk: &Arc<CheckpointDesk>,
    transport: Arc<dyn Transport>,
    keys: Arc<dyn SecretStore>,
) -> Arc<dyn RunDriver> {
    let config = agent_model(app, Provider::Xai)
        .map_or_else(|| ResponsesConfig::xai(GROK_MODEL), responses_config);
    match agent_file_tools(app, desk, Provider::Xai) {
        Ok(tools) => Arc::new(ResponsesAgent::new(transport, keys, config, tools)),
        Err(reason) => {
            eprintln!("corral: Grok has no tools: {reason}");
            Arc::new(ResponsesDriver::new(transport, keys, config))
        }
    }
}

/// GPT on the ChatGPT plan (TP-SUB-07), with its file tools, or with none without its folder or
/// a usable policy. Each run uses the model saved in the agent editor while the plan lists it,
/// else the plan's first (TP-SUB-08).
fn gpt_agent(
    app: &AppHandle,
    desk: &Arc<CheckpointDesk>,
    transport: Arc<dyn Transport>,
    plan: Arc<ChatGptPlan>,
) -> Arc<dyn RunDriver> {
    let keys: Arc<dyn KeySource> = plan.clone();
    let gpt = agent_for(Provider::OpenAi);
    let preferred = match app.state::<AgentSettings>().plan_model(&gpt.id) {
        Ok(model) => model,
        Err(reason) => {
            eprintln!(
                "corral: {}'s model can't be used, so it runs on the plan's first: {reason}",
                gpt.name
            );
            None
        }
    };
    let agent = match agent_file_tools(app, desk, Provider::OpenAi) {
        Ok(tools) => {
            let build: PlanBuild = Arc::new(move |config: ResponsesConfig| {
                let agent = ResponsesAgent::with_keys(
                    transport.clone(),
                    keys.clone(),
                    config,
                    tools.clone(),
                );
                Arc::new(agent) as Arc<dyn RunDriver>
            });
            // The tool loop stops at safe points, so its cards offer Pause.
            PlanAgent::new(plan, preferred, build, true)
        }
        Err(reason) => {
            eprintln!("corral: GPT has no tools: {reason}");
            let build: PlanBuild = Arc::new(move |config: ResponsesConfig| {
                let driver = ResponsesDriver::with_keys(transport.clone(), keys.clone(), config);
                Arc::new(driver) as Arc<dyn RunDriver>
            });
            PlanAgent::new(plan, preferred, build, false)
        }
    };
    Arc::new(agent)
}

/// Claude with its file tools, or with none without its folder or a usable policy.
fn claude_with_tools(
    app: &AppHandle,
    desk: &Arc<CheckpointDesk>,
    transport: Arc<dyn Transport>,
    keys: Arc<dyn SecretStore>,
) -> Arc<dyn RunDriver> {
    let config = agent_model(app, Provider::Anthropic)
        .map_or_else(|| MessagesConfig::anthropic(CLAUDE_MODEL), messages_config);
    match agent_file_tools(app, desk, Provider::Anthropic) {
        Ok(tools) => claude_agent(app, transport, keys, config, tools),
        Err(reason) => {
            eprintln!("corral: Claude has no tools: {reason}");
            Arc::new(MessagesDriver::new(transport, keys, config))
        }
    }
}

/// The model `provider`'s agent uses: its saved one, or its default when its settings can't be
/// used (it can't spend then: its cap is $0).
fn agent_model(app: &AppHandle, provider: Provider) -> Option<&'static ModelChoice> {
    let agent = agent_for(provider);
    match app.state::<AgentSettings>().load(&agent.id) {
        Ok(setup) => setup.model,
        Err(reason) => {
            eprintln!("corral: {}'s settings can't be used: {reason}", agent.name);
            models(provider).first()
        }
    }
}

/// Claude with its file tools, and with web search when the user turned it on in the main window
/// (TP-PROV-13): the switch is read as each run starts. Without a way to allow it, Claude runs
/// without search.
fn claude_agent(
    app: &AppHandle,
    transport: Arc<dyn Transport>,
    keys: Arc<dyn SecretStore>,
    config: MessagesConfig,
    tools: ToolSetup,
) -> Arc<dyn RunDriver> {
    let plain: Arc<dyn RunDriver> = Arc::new(MessagesAgent::new(
        transport.clone(),
        keys.clone(),
        config.clone(),
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
            ..config
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

/// The file tools over `~/Corral/<agent name>` for `provider`'s agent, under its saved policy
/// (TP-PED-06), with checkpoints on `desk`. A saved policy that can't be used means no tools:
/// it fails closed rather than falling back to the defaults.
fn agent_file_tools(
    app: &AppHandle,
    desk: &Arc<CheckpointDesk>,
    provider: Provider,
) -> Result<ToolSetup, String> {
    let home = app
        .path()
        .home_dir()
        .map_err(|e| format!("no home folder: {e}"))?;
    let agent = agent_for(provider);
    let policy = app
        .state::<PolicyEditor>()
        .policies()
        .load(&agent.id)
        .map_err(|e| {
            format!(
                "{}'s policy can't be used, so it has no tools: {e}",
                agent.name
            )
        })?;
    // The folder follows the agent's name (TP-ANM-03).
    let folder = app.state::<AgentNames>().folder(&home, &agent.id);
    file_tools_under(&folder, Arc::clone(desk), policy)
}

/// How far back launch reads spend: past the start of this month in any time zone.
const SPEND_LOOKBACK_MS: u64 = 32 * 24 * 60 * 60 * 1_000;

/// Unique run and session ID suffixes: the start time plus a counter.
fn id_source() -> Arc<dyn Fn() -> String + Send + Sync> {
    let started = SystemClock.now_ms();
    let next = AtomicU64::new(1);
    Arc::new(move || format!("{started:x}-{}", next.fetch_add(1, Ordering::Relaxed)))
}

/// Keeps the corral in step with the event log.
pub struct CorralFeed {
    views: Mutex<CorralState>,
    app: AppHandle,
    clock: Arc<dyn Clock>,
}

impl CorralFeed {
    /// Whether `agent`'s card shows `checkpoint` now (the core decides, from the log).
    fn waits_on(&self, agent: &AgentId, checkpoint: &CheckpointId) -> bool {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no checkpoint can be answered");
            return false;
        };
        views.cards.waits_on(agent, checkpoint)
    }

    /// `agent`'s card showing `checkpoint` (one waiting on it, as the user stepped to it) and
    /// the name its drop shows, for a Touch ID prompt.
    fn card_and_name(
        &self,
        agent: &AgentId,
        checkpoint: &CheckpointId,
    ) -> Option<(RunCard, String)> {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no card can be read");
            return None;
        };
        let card = views
            .cards
            .card_showing(agent, checkpoint, self.clock.now_ms())?;
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
        let now = self.clock.now_ms();
        if views.set_agents(cards, linked, now) {
            self.send(views.snapshot(now));
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
        if views.seed_usage(deploys, now) {
            self.send(views.snapshot(now));
        }
    }

    /// Swift's report that the corral turned idle or busy (TP-USAGE-10). Idle with an order
    /// waiting and nothing waiting on the user, the new order is sent.
    fn corral_idle(&self, idle: bool) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        let now = self.clock.now_ms();
        if views.corral_idle(idle, now) {
            self.send(views.snapshot(now));
        }
    }

    /// Shows `agent`'s logo from `path`, if it's new.
    fn set_logo(&self, agent: AgentId, path: String) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        if views.set_logo(agent, path) {
            self.send(views.snapshot(self.clock.now_ms()));
        }
    }

    /// Sends the current roster from the main thread (during `setup`).
    fn publish_now(&self) -> Result<(), native::NativeError> {
        let views = self
            .views
            .lock()
            .map_err(|_| native::NativeError::Poisoned)?;
        native::set_roster(&views.snapshot(self.clock.now_ms()))
    }

    /// Updates the views and, when a drop or a card changed, sends the new roster to Swift. The
    /// roster is queued for the main thread while the views are still locked, so updates reach
    /// Swift in the order they happened.
    fn observe(&self, event: &Event) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned; the corral stops updating");
            return;
        };
        let now = self.clock.now_ms();
        let (changed, posts) = views.observe(event, now);
        if changed {
            self.send(views.snapshot(now));
        }
        drop(views);
        for notice in posts {
            self.notify(&notice);
        }
    }

    /// Posts a checkpoint notification (TP-CHK-22). If it can't be posted, its card still shows
    /// the checkpoint.
    fn notify(&self, notice: &Notice) {
        let request = NotificationRequest::from(notice);
        let checkpoint = notice.checkpoint.clone();
        let posted = self.app.run_on_main_thread(move || {
            if let Err(e) = native::notify(&request) {
                eprintln!(
                    "corral: couldn't post the notification for checkpoint {checkpoint}: {e}"
                );
            }
        });
        if let Err(e) = posted {
            eprintln!("corral: couldn't reach the main thread for a notification: {e}");
        }
    }

    /// The agents whose cards offer Pause.
    fn set_pausable(&self, agents: impl IntoIterator<Item = AgentId>) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so no card offers Pause");
            return;
        };
        views.cards.set_pausable(agents);
    }

    /// Pause (`asked`) or Resume was pressed on `agent`'s card and the supervisor took it: the
    /// card reads Pausing until the run reaches its safe point, or Working again (TP-PAUSE-01).
    fn ask_pause(&self, agent: &AgentId, asked: bool) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so {agent}'s card wasn't updated");
            return;
        };
        if views.cards.ask_pause(agent, asked) {
            self.send(views.snapshot(self.clock.now_ms()));
        }
    }

    /// What a click on a checkpoint notification means, from the core (TP-CHK-23).
    fn note_click(&self, checkpoint: &CheckpointId, nonce: &str, click: NoteClick) -> NoteAnswer {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so the notification click was dropped");
            return NoteAnswer::Stale;
        };
        views.notes.click(checkpoint, nonce, click)
    }

    /// Swift couldn't show a checkpoint notification.
    fn note_failed(&self, checkpoint: &CheckpointId, trouble: NoteTrouble) {
        let Ok(mut views) = self.views.lock() else {
            eprintln!("corral: the corral view is poisoned, so a notification failure was dropped");
            return;
        };
        views.notes.failed(checkpoint, trouble);
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
                        feed.send(views.snapshot(feed.clock.now_ms()));
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

/// A task card in the trace tree opens its run in the main window's trace viewer (TP-TRACE-05).
impl CorralFeed {
    fn open_trace(&self, agent: &AgentId) {
        let run = match self.views.lock() {
            Ok(views) => views.cards.run_of(agent).cloned(),
            Err(_) => {
                eprintln!("corral: the corral's state is poisoned, so its traces can't be opened");
                return;
            }
        };
        // No run to show (it was never started, or the card was cleared): nothing to open.
        let Some(run) = run else { return };
        self.app.state::<Arc<Traces>>().open_run(run);
        crate::show_main_window(&self.app);
        // The viewer also takes the run when the window comes forward, so a failed nudge only
        // delays it.
        if let Some(window) = self.app.get_webview_window("main")
            && let Err(e) = window.eval(crate::traces::OPEN_RUN_SCRIPT)
        {
            eprintln!("corral: couldn't tell the trace viewer to open the run: {e}");
        }
    }
}

/// The prompt bar (docs/07 §6.1, TP-BAR-08): the core decides; these read the corral's state and
/// pass the answer to Swift.
impl CorralFeed {
    /// The agents the prompt bar knows now, in the corral's order with their status.
    fn bar_agents(&self) -> Vec<prompt_bar::BarAgent> {
        let Ok(views) = self.views.lock() else {
            eprintln!("corral: the corral's state is poisoned, so the prompt bar knows no agents");
            return Vec::new();
        };
        let now = self.clock.now_ms();
        let settings = views.order.settings(&views.all_cards);
        prompt_bar_feed::bar_agents(&settings, |id| views.cards.card(id, now).map(|c| c.status))
    }

    /// Shows the prompt bar's answer, on the main thread, with each suggestion's logo.
    fn send_bar(&self, answer: PromptBarSnapshot) {
        let answer = match self.views.lock() {
            Ok(views) => answer.with_logos(|id| views.logos.get(id).cloned()),
            Err(_) => {
                eprintln!(
                    "corral: the corral's state is poisoned, so the prompt bar shows no logos"
                );
                answer
            }
        };
        let posted = self.app.run_on_main_thread(move || {
            if let Err(e) = native::prompt_bar(&answer) {
                eprintln!("corral: couldn't update the prompt bar: {e}");
            }
        });
        if let Err(e) = posted {
            eprintln!("corral: couldn't reach the main thread to update the prompt bar: {e}");
        }
    }
}

/// The main window's Corral section (TP-USAGE-15). The corral's order decides; a change is saved
/// and the drops re-placed at once.
impl CorralControl for CorralFeed {
    fn settings(&self) -> Result<CorralSettingsView, String> {
        let views = self.views.lock().map_err(|_| POISONED.to_owned())?;
        Ok(views.order.settings(&views.all_cards))
    }

    fn choose_shown(&self, ids: Vec<String>) -> Result<CorralSettingsView, String> {
        let mut views = self.views.lock().map_err(|_| POISONED.to_owned())?;
        let picks: Vec<AgentId> = ids.iter().map(|id| AgentId::from(id.as_str())).collect();
        views.order.choose_shown(&picks)?;
        save_order(&mut views.order);
        let cards = views.all_cards.clone();
        let placed = views.order.place(cards, self.clock.now_ms());
        if views.corral.set_agents(placed.cards) {
            self.send(views.snapshot(self.clock.now_ms()));
        }
        Ok(views.order.settings(&views.all_cards))
    }

    fn keep_order_fixed(&self, keep: bool) -> Result<CorralSettingsView, String> {
        let mut views = self.views.lock().map_err(|_| POISONED.to_owned())?;
        views.order.set_keep_fixed(keep);
        save_order(&mut views.order);
        Ok(views.order.settings(&views.all_cards))
    }
}

/// What the Corral section says when the corral view is poisoned.
const POISONED: &str = "The corral stopped updating after an error. Restart Corral to change it.";

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
        Intent::OpenTrace(agent) => corral.feed.open_trace(&agent),
        Intent::PromptBarEdited(edit) => {
            let agents = corral.feed.bar_agents();
            corral
                .feed
                .send_bar(prompt_bar_feed::edited(&edit, &agents));
        }
        Intent::PromptBarSend { edit, how } => {
            let agents = corral.feed.bar_agents();
            let feed = Arc::clone(&corral.feed);
            let Some(controls) = corral.controls.clone() else {
                feed.send_bar(prompt_bar_feed::tasks_off(&edit, &agents));
                return;
            };
            tauri::async_runtime::spawn(async move {
                let answer = prompt_bar_feed::sent(&edit, how, &agents, |agent, task| {
                    controls.deploy(agent, task)
                });
                feed.send_bar(answer);
            });
        }
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
        Intent::Pause(agent) => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            match controls.pause(&agent) {
                Ok(_) => corral.feed.ask_pause(&agent, true),
                Err(e) => eprintln!("corral: pause for {agent} refused: {e}"),
            }
        }
        Intent::Resume(agent) => {
            let Some(controls) = tasks(&corral, &agent) else {
                return;
            };
            match controls.resume(&agent) {
                Ok(_) => corral.feed.ask_pause(&agent, false),
                Err(e) => eprintln!("corral: resume for {agent} refused: {e}"),
            }
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
        Intent::NotificationAction {
            checkpoint,
            nonce,
            action,
        } => corral.note_click(&checkpoint, &nonce, action.into()),
        Intent::NotificationFailed { checkpoint, reason } => {
            eprintln!(
                "corral: checkpoint {checkpoint}'s notification wasn't shown ({reason:?}); its card shows it"
            );
            corral.feed.note_failed(&checkpoint, reason.into());
        }
        Intent::OpenAgent(_) | Intent::Deploy(_) | Intent::SummonPromptBar => {
            eprintln!("corral: intent {intent:?} (handled in a later milestone)");
        }
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
