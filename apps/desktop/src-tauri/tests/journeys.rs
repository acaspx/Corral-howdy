//! End-to-end journeys as transcripts (Spike S2, `docs/04-test-plan.md` §6). A journey is played
//! through the real core: the supervisor, the corral's controls and what the corral shows
//! ([`CorralState`], the same state the app's feed holds), with `FakeAdapter` scripts in place of
//! providers and a clock that moves only with tokio's paused time. Each user gesture is the
//! intent JSON Swift sends, decoded by the bridge and handled as the app's `on_intent` does;
//! each change the core makes is recorded as the roster Swift gets, and each prompt bar answer
//! as the bar's wire.
//!
//! The recording is the journey's transcript, checked in beside the bridge fixtures
//! (`native/CorralNative/Fixtures/journeys/`). This test fails when the core no longer produces
//! it, so a change shows up as a reviewed diff; `CorralJourneyTests.swift` replays it into
//! CorralNative, performs each gesture on the corral's own models and checks the intent and what
//! the corral shows. Regenerate with
//! `CORRAL_WRITE_FIXTURES=1 cargo test -p corral-desktop --test journeys`.
//!
//! No keys and no network (AGENTS.md rule 7). Tools go through the gateway and the policy, with
//! checkpoints answered on the desk the app uses (rule 4: no bypass).

use corral_adapters::catalog::{AgentRouter, agent_for};
use corral_adapters::chatgpt::ChatGptPlan;
use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_adapters::linked::LinkedSettings;
use corral_adapters::logos::{LogoError, LogoFetcher, LogoSource};
use corral_adapters::transport::{Form, FormTransport, TransportError};
use corral_adapters::verify::ModelsProbe;
use corral_core::budget::{BudgetConfig, Price};
use corral_core::clock::Clock;
use corral_core::controls::CorralControls;
use corral_core::corral_view::AgentCard;
use corral_core::domain::ActionClass;
use corral_core::event::Event;
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::supervisor::{
    EventLog, LogError, RunDriver, Supervisor, SupervisorConfig, WatchedLog,
};
use corral_core::usage_rank::Deploy;
use corral_desktop_lib::agent_editor::{AgentEdit, AgentEditor};
use corral_desktop_lib::agent_names::AgentNames;
use corral_desktop_lib::agent_settings::AgentSettings;
use corral_desktop_lib::bridge::{Intent, decode_intent};
use corral_desktop_lib::corral_order::{CorralOrder, CorralSettingsView};
use corral_desktop_lib::corral_settings::{CorralControl, CorralSettings};
use corral_desktop_lib::corral_state::{
    CorralAgents, CorralState, RUN_CAP_MICRO_USD, SOFT_LIMIT_MICRO_USD, corral_for,
};
use corral_desktop_lib::keys;
use corral_desktop_lib::logos::{LogoStore, fetch_missing, wanted};
use corral_desktop_lib::prompt_bar_feed;
use corral_desktop_lib::traces::Traces;
use corral_policy::{Policy, Profile};
use corral_secrets::{MemoryPlanStore, MemoryStore, Provider, SecretStore};
use corral_store::Store;
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 2026-11-26 09:00 Pacific, in ms.
const START_MS: u64 = 1_796_144_400_000;

/// Unix time that moves with tokio's paused clock, so transcripts are the same on every run.
struct PausedClock(tokio::time::Instant);

impl Clock for PausedClock {
    fn now_ms(&self) -> u64 {
        let elapsed = u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX);
        START_MS.saturating_add(elapsed)
    }
}

/// The event log: kept in memory, in order.
#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

/// A tool that answers at once.
struct Plain {
    name: &'static str,
    class: ActionClass,
    answer: &'static str,
}

impl Tool for Plain {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["to"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, _args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok(self.answer.to_owned()) })
    }
}

/// Files and email behind the gate, under the default policy: writing a file asks, at normal
/// risk; sending an email asks, at high risk.
fn tools(desk: &Arc<CheckpointDesk>, journey: &str) -> ToolSetup {
    let mut gateway = ToolGateway::new();
    for tool in [
        Plain {
            name: "fs.write",
            class: ActionClass::WriteLocal,
            answer: "Saved.",
        },
        Plain {
            name: "email.send",
            class: ActionClass::CommsSend,
            answer: "Sent.",
        },
    ] {
        gateway.register(Arc::new(tool)).expect("register");
    }
    let n = Arc::new(AtomicU64::new(0));
    let prefix = journey.to_owned();
    ToolSetup {
        policy: Policy::new(Profile::default()),
        gateway: Arc::new(gateway),
        desk: Arc::clone(desk),
        ids: Arc::new(move || format!("{prefix}-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
    }
}

/// The core as the app wires it, recording every roster and prompt bar answer it sends.
struct Core {
    state: Arc<Mutex<CorralState>>,
    controls: CorralControls,
    desk: Arc<CheckpointDesk>,
    sent: Arc<Mutex<Vec<Value>>>,
    bars: Mutex<Vec<Value>>,
    /// The last roster sent, for a step that sends none.
    last: Mutex<Value>,
    clock: Arc<PausedClock>,
    /// The main window's Corral section, reaching this corral as the app's does.
    settings: CorralSettings,
    /// The rest of the app, for a journey that starts from the keys (J1).
    app: Option<AppSide>,
}

/// What the app's feed does for the Corral section's commands (`impl CorralControl for
/// CorralFeed`): the corral's state decides, and a change sends the roster.
struct StateControl {
    state: Arc<Mutex<CorralState>>,
    sent: Arc<Mutex<Vec<Value>>>,
    clock: Arc<PausedClock>,
}

impl CorralControl for StateControl {
    fn settings(&self) -> Result<CorralSettingsView, String> {
        Ok(self.state.lock().expect("state").settings())
    }

    fn choose_shown(&self, ids: Vec<String>) -> Result<CorralSettingsView, String> {
        let mut state = self.state.lock().expect("state");
        let now = self.clock.now_ms();
        if state.choose_shown(&ids, now)? {
            self.sent
                .lock()
                .expect("sent")
                .push(roster_json(&state, now));
        }
        Ok(state.settings())
    }

    fn keep_order_fixed(&self, keep: bool) -> Result<CorralSettingsView, String> {
        Ok(self.state.lock().expect("state").keep_order_fixed(keep))
    }
}

fn roster_json(state: &CorralState, now_ms: u64) -> Value {
    let bytes = state.snapshot(now_ms).to_bytes().expect("roster");
    serde_json::from_slice(&bytes).expect("roster JSON")
}

impl Core {
    fn new(driver: Arc<dyn RunDriver>, desk: Arc<CheckpointDesk>, agents: &[Provider]) -> Self {
        Self::with_cards(driver, desk, cards(agents))
    }

    /// The core with these agents, some of them stand-ins beyond today's catalog.
    fn with_cards(
        driver: Arc<dyn RunDriver>,
        desk: Arc<CheckpointDesk>,
        agents: Vec<AgentCard>,
    ) -> Self {
        let runnable = agents.iter().map(|card| card.id.clone()).collect();
        Self::with_agents(
            driver,
            desk,
            agents,
            runnable,
            None,
            Arc::new(MemLog::default()),
        )
    }

    /// The core as the app starts it from its keys, its sign-in and the agents' names
    /// (`surface::start`), with the app's soft limit for each run.
    fn with_app(driver: Arc<dyn RunDriver>, desk: Arc<CheckpointDesk>, app: AppSide) -> Self {
        let agents = app.agents();
        let limits = (
            BudgetConfig {
                per_run_cap_micro_usd: RUN_CAP_MICRO_USD,
            },
            SOFT_LIMIT_MICRO_USD,
        );
        let store: Arc<dyn EventLog> = Arc::clone(&app.store) as Arc<dyn EventLog>;
        let mut core = Self::with_agents(
            driver,
            desk,
            agents.cards,
            agents.runnable,
            Some(limits),
            store,
        );
        core.app = Some(app);
        core
    }

    fn with_agents(
        driver: Arc<dyn RunDriver>,
        desk: Arc<CheckpointDesk>,
        agents: Vec<AgentCard>,
        runnable: Vec<AgentId>,
        limits: Option<(BudgetConfig, u64)>,
        events: Arc<dyn EventLog>,
    ) -> Self {
        let clock = Arc::new(PausedClock(tokio::time::Instant::now()));
        let nonce = Arc::new(AtomicU64::new(1));
        let state = CorralState::open(
            CorralOrder::open(None),
            agents.clone(),
            LinkedSettings::default(),
            clock.now_ms(),
        )
        .with_nonces(Box::new(move || {
            format!("nonce-{}", nonce.fetch_add(1, Ordering::SeqCst))
        }));
        let state = Arc::new(Mutex::new(state));
        let sent = Arc::new(Mutex::new(Vec::new()));
        // What the app's feed does with each logged event: update the corral, send the roster
        // if it changed.
        let log = {
            let (state, sent, clock) = (Arc::clone(&state), Arc::clone(&sent), Arc::clone(&clock));
            Arc::new(WatchedLog::new(events, move |event: &Event| {
                let mut state = state.lock().expect("state");
                let now = clock.now_ms();
                let (changed, _notices) = state.observe(event, now);
                if changed {
                    sent.lock().expect("sent").push(roster_json(&state, now));
                }
            }))
        };
        let (budget, soft_limit) = match limits {
            Some((budget, soft)) => (budget, Some(soft)),
            None => (BudgetConfig::default(), None),
        };
        let supervisor = Supervisor::with_budget(
            SupervisorConfig::default(),
            budget,
            driver,
            log,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        supervisor.set_soft_limit(soft_limit);
        let ids = Arc::new(AtomicU64::new(1));
        let controls = CorralControls::new(
            supervisor,
            runnable,
            Arc::new(move || format!("j-{}", ids.fetch_add(1, Ordering::SeqCst))),
        );
        let settings = CorralSettings::default();
        settings.attach(Arc::new(StateControl {
            state: Arc::clone(&state),
            sent: Arc::clone(&sent),
            clock: Arc::clone(&clock),
        }));
        let opening = roster_json(&state.lock().expect("state"), clock.now_ms());
        let core = Self {
            state,
            controls,
            desk,
            sent,
            bars: Mutex::new(Vec::new()),
            last: Mutex::new(opening.clone()),
            clock,
            settings,
            app: None,
        };
        // The roster the corral opens with (the app's `publish_now`).
        core.sent.lock().expect("sent").push(opening);
        core
    }

    /// A gesture's intent, as Swift sends it, through the bridge and handled as the app's
    /// `on_intent` does.
    async fn intent(&self, wire: &Value) {
        let bytes = serde_json::to_vec(wire).expect("intent");
        let intent = decode_intent(&bytes).expect("the bridge takes it");
        let answer = match intent {
            Intent::DeployTask { agent, prompt } => {
                self.controls.deploy(&agent, &prompt).expect("deployed");
                None
            }
            Intent::SendBack(agent) => {
                self.controls.send_back(&agent).expect("sent back");
                None
            }
            Intent::PromptBarEdited(edit) => {
                let agents = self.bar_agents();
                Some(prompt_bar_feed::edited(&edit, &agents))
            }
            Intent::PromptBarSend { edit, how } => {
                let agents = self.bar_agents();
                Some(prompt_bar_feed::sent(&edit, how, &agents, |agent, task| {
                    self.controls.deploy(agent, task)
                }))
            }
            Intent::ApproveCheckpoint {
                agent,
                checkpoint,
                scope,
                how,
            } => {
                self.answer(&agent, &checkpoint, Answer::Approve { how, scope });
                None
            }
            Intent::DenyCheckpoint {
                agent,
                checkpoint,
                note,
            } => {
                self.answer(&agent, &checkpoint, Answer::Deny { note });
                None
            }
            Intent::EditCheckpoint {
                agent,
                checkpoint,
                changes,
                how,
            } => {
                let args = self
                    .state
                    .lock()
                    .expect("state")
                    .edited_args(&agent, &checkpoint, &changes)
                    .expect("the core takes the edit");
                self.answer(&agent, &checkpoint, Answer::Edit { how, args });
                None
            }
            Intent::StopAll => {
                let report = self.controls.stop_all().await;
                assert_eq!(report.forced, 0, "every run stopped when asked");
                None
            }
            Intent::CorralIdle(idle) => {
                // The app's `corral_idle`: an order waiting takes once the corral is idle.
                let mut state = self.state.lock().expect("state");
                let now = self.clock.now_ms();
                if state.corral_idle(idle, now) {
                    self.sent
                        .lock()
                        .expect("sent")
                        .push(roster_json(&state, now));
                }
                None
            }
            other @ (Intent::OpenAgent(_)
            | Intent::Deploy(_)
            | Intent::SummonPromptBar
            | Intent::NotificationAction { .. }
            | Intent::NotificationFailed { .. }
            | Intent::AuthResult { .. }
            | Intent::Stop(_)
            | Intent::Pause(_)
            | Intent::Resume(_)
            | Intent::Retry(_)
            | Intent::ShowLinked { .. }
            | Intent::HideLinked(_)
            | Intent::OpenLinked(_)
            | Intent::ApproveWithTouchId { .. }) => panic!("not in a journey yet: {other:?}"),
            Intent::OpenTrace(agent) => {
                // The app's `open_trace`: the run the card follows goes to the trace viewer,
                // which takes it when the main window comes forward.
                let app = self.app.as_ref().expect("a journey with the trace viewer");
                let run = self.state.lock().expect("state").run_of(&agent);
                app.traces.open_run(run.expect("the card has a run"));
                None
            }
        };
        if let Some(answer) = answer {
            let answer = self.state.lock().expect("state").with_bar_logos(answer);
            let bytes = answer.to_bytes().expect("the bar's answer");
            let bar = serde_json::from_slice(&bytes).expect("the bar's JSON");
            self.bars.lock().expect("bars").push(bar);
        }
        settle().await;
    }

    fn bar_agents(&self) -> Vec<corral_core::prompt_bar::BarAgent> {
        self.state
            .lock()
            .expect("state")
            .bar_agents(self.clock.now_ms())
    }

    /// The app's `Corral::answer`: only the checkpoint the card shows may be answered from it,
    /// and the desk takes each one once. The state's lock is let go first: the answer is logged.
    fn answer(&self, agent: &AgentId, checkpoint: &CheckpointId, answer: Answer) {
        let shown = self
            .state
            .lock()
            .expect("state")
            .waits_on(agent, checkpoint);
        assert!(shown, "{checkpoint} doesn't wait on {agent}'s card");
        self.desk
            .answer(checkpoint, answer)
            .expect("the desk takes it");
    }

    /// A key connected or removed: the corral's agents change (`Corral::refresh`).
    fn set_agents(&self, agents: &[Provider]) {
        self.set_cards(cards(agents));
    }

    /// The corral's agents change to these, some of them stand-ins.
    fn set_cards(&self, agents: Vec<AgentCard>) {
        self.controls
            .set_agents(agents.iter().map(|card| card.id.clone()));
        let mut state = self.state.lock().expect("state");
        let now = self.clock.now_ms();
        if state.set_agents(agents, LinkedSettings::default(), now) {
            self.sent
                .lock()
                .expect("sent")
                .push(roster_json(&state, now));
        }
    }

    /// At launch, once the store is open: the deploys of the last 90 days (`seed_usage`).
    fn seed(&self, deploys: Vec<Deploy>) {
        let mut state = self.state.lock().expect("state");
        let now = self.clock.now_ms();
        if state.seed_usage(deploys, now) {
            self.sent
                .lock()
                .expect("sent")
                .push(roster_json(&state, now));
        }
    }

    /// The main window's Corral section opens: the `corral_settings` command.
    fn window_settings(&self) -> Value {
        let view = self.settings.settings().expect("the settings");
        json!({"command": "corral_settings", "args": {}, "answer": view})
    }

    /// A box ticked or unticked in the Corral section, as its `toggle` asks the core: the shown
    /// agents in the section's order, with `agent` flipped (`set_corral_shown`).
    fn window_toggle(&self, agent: &str) -> Value {
        let view = self.settings.settings().expect("the settings");
        let ids: Vec<String> = view
            .agents
            .iter()
            .filter(|a| if a.id == agent { !a.shown } else { a.shown })
            .map(|a| a.id.clone())
            .collect();
        let answer = self
            .settings
            .choose_shown(ids.clone())
            .expect("the core takes it");
        json!({"command": "set_corral_shown", "args": {"ids": ids}, "answer": answer})
    }

    /// The app's `Corral::refresh` after a key or a name changed: the corral's agents follow the
    /// keys, and each new provider's logo is fetched (here, from a fake) and shown.
    async fn refresh(&self) {
        let app = self.app.as_ref().expect("a journey with the app");
        let linked = LinkedSettings::default();
        let connected = app.connected();
        let agents = app.agents();
        self.controls.set_agents(agents.runnable);
        {
            let mut state = self.state.lock().expect("state");
            let now = self.clock.now_ms();
            if state.set_agents(agents.cards, linked, now) {
                self.sent
                    .lock()
                    .expect("sent")
                    .push(roster_json(&state, now));
            }
        }
        // `show_logos`: what's missing is fetched, and each saved logo shows.
        let want = wanted(&connected, &linked);
        let ids: Vec<_> = want.iter().map(|(id, _)| *id).collect();
        for (id, result) in fetch_missing(&FakeLogos, &app.logos, &ids).await {
            let agent = want
                .iter()
                .find(|(w, _)| *w == id)
                .map(|(_, a)| a.clone())
                .expect("wanted");
            let path = result.expect("the logo is saved");
            let mut state = self.state.lock().expect("state");
            if state.set_logo(agent, path.to_string_lossy().into_owned()) {
                let now = self.clock.now_ms();
                self.sent
                    .lock()
                    .expect("sent")
                    .push(roster_json(&state, now));
            }
        }
        settle().await;
    }

    /// The rosters sent since the last step.
    fn take_sent(&self) -> Vec<Value> {
        let sent = std::mem::take(&mut *self.sent.lock().expect("sent"));
        if let Some(last) = sent.last() {
            *self.last.lock().expect("last") = last.clone();
        }
        sent
    }

    /// The prompt bar answers sent since the last step.
    fn take_bars(&self) -> Vec<Value> {
        std::mem::take(&mut *self.bars.lock().expect("bars"))
    }

    /// The checkpoint waiting on `agent`'s card, as the latest roster shows it.
    fn waiting(&self, agent: &str) -> String {
        let latest = self
            .sent
            .lock()
            .expect("sent")
            .last()
            .cloned()
            .unwrap_or_else(|| self.last.lock().expect("last").clone());
        latest["cards"]
            .as_array()
            .and_then(|cards| cards.iter().find(|c| c["id"] == agent))
            .and_then(|card| card["checkpoint"]["id"].as_str())
            .unwrap_or_else(|| panic!("nothing waits on {agent}'s card"))
            .to_owned()
    }
}

fn cards(agents: &[Provider]) -> Vec<corral_core::corral_view::AgentCard> {
    agents.iter().map(|p| agent_for(*p)).collect()
}

async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

async fn wait(seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    settle().await;
}

/// One step of a transcript.
struct Step {
    say: &'static str,
    gesture: Value,
    intent: Value,
    rosters: Vec<Value>,
    bars: Vec<Value>,
    /// A main-window step: its gesture, the command it calls and the core's answer.
    window: Value,
    /// Only the step's last roster is kept: its runs end in no fixed order.
    settled: bool,
    expect: Value,
}

impl Step {
    fn to_json(&self) -> Value {
        let mut step = json!({
            "say": self.say,
            "gesture": self.gesture,
            "intent": self.intent,
            "rosters": self.rosters,
            "expect": self.expect,
        });
        // Only a step the prompt bar answered has bars, and only a main-window step a window.
        if !self.bars.is_empty() {
            step["bars"] = json!(self.bars);
        }
        if !self.window.is_null() {
            step["window"] = self.window.clone();
        }
        if self.settled {
            step["settled"] = json!(true);
        }
        step
    }
}

/// What the corral should show after a step: its shape, its drops in slot order with their
/// status, and the trace tree's columns. The core's last roster must say the same.
fn expect(shape: &str, drops: &[(&str, &str)], columns: &[&str]) -> Value {
    json!({
        "shape": shape,
        "drops": drops.iter().map(|(id, status)| json!({"id": id, "status": status})).collect::<Vec<_>>(),
        "columns": columns,
    })
}

/// `expected`, with each trace column's status (its wire's colour).
fn with_column_status(mut expected: Value, statuses: &[(&str, &str)]) -> Value {
    let map: serde_json::Map<String, Value> = statuses
        .iter()
        .map(|(id, status)| ((*id).to_owned(), json!(status)))
        .collect();
    expected["columnStatus"] = Value::Object(map);
    expected
}

/// `expected`, with the approval box on `agent`'s card: the checkpoint's class and risk, and
/// text its detail must hold (where it goes and what it says).
fn with_approval(
    mut expected: Value,
    agent: &str,
    class: &str,
    risk: &str,
    detail: &[&str],
) -> Value {
    expected["approval"] = json!({"agent": agent, "class": class, "risk": risk, "detail": detail});
    expected
}

/// `expected`, with what the prompt bar shows: its chips, its button and how the last send went
/// (`None` for no send).
fn with_bar(mut expected: Value, chips: &[&str], button: &str, outcome: Option<&str>) -> Value {
    expected["bar"] = json!({"chips": chips, "button": button, "outcome": outcome});
    expected
}

/// A bar answer's outcome, as its kind on the wire (`sent`, `note` or `refused`), or `null`
/// for an edit's answer.
fn outcome_word(outcome: &Value) -> Value {
    outcome.get("kind").cloned().unwrap_or(Value::Null)
}

/// Records a step, checking the core's last roster and bar answer against `expected`.
fn checked(core: &Core, say: &'static str, gesture: Value, intent: Value, expected: Value) -> Step {
    let rosters = core.take_sent();
    let bars = core.take_bars();
    let last = core.last.lock().expect("last").clone();
    let drops: Vec<Value> = last["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|a| json!({"id": a["id"], "status": a["status"]}))
        .collect();
    assert_eq!(Value::Array(drops), expected["drops"], "{say}: the drops");
    let columns: Vec<Value> = last["trace"]["columns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids: Vec<Value> = columns.iter().map(|c| c["id"].clone()).collect();
    assert_eq!(
        Value::Array(ids),
        expected["columns"],
        "{say}: the trace columns"
    );
    if let Some(statuses) = expected["columnStatus"].as_object() {
        for (id, status) in statuses {
            let column = columns.iter().find(|c| c["id"] == id.as_str());
            assert_eq!(
                column.map(|c| &c["status"]),
                Some(status),
                "{say}: {id}'s column"
            );
        }
    }
    if expected["approval"].is_object() {
        let approval = &expected["approval"];
        let card = last["cards"]
            .as_array()
            .and_then(|cards| cards.iter().find(|c| c["id"] == approval["agent"]))
            .unwrap_or_else(|| panic!("{say}: no card"));
        let checkpoint = &card["checkpoint"];
        assert_eq!(checkpoint["class"], approval["class"], "{say}: the class");
        assert_eq!(checkpoint["risk"], approval["risk"], "{say}: the risk");
        let detail = checkpoint["detail"].as_str().unwrap_or_default();
        for text in approval["detail"].as_array().expect("detail") {
            let text = text.as_str().expect("text");
            assert!(
                detail.contains(text),
                "{say}: the detail says {text:?}: {detail}"
            );
        }
    }
    if expected["bar"].is_object() {
        let bar = bars
            .last()
            .unwrap_or_else(|| panic!("{say}: the bar wasn't answered"));
        assert_eq!(
            bar["view"]["chips"], expected["bar"]["chips"],
            "{say}: the chips"
        );
        assert_eq!(
            bar["view"]["button"]["label"], expected["bar"]["button"],
            "{say}: the button"
        );
        assert_eq!(
            outcome_word(&bar["outcome"]),
            expected["bar"]["outcome"],
            "{say}: the outcome: {}",
            bar["outcome"]
        );
    }
    let agents = last["agents"].as_array().expect("agents");
    if let Some(names) = expected["names"].as_object() {
        for (id, name) in names {
            let drop = agents.iter().find(|a| a["id"] == id.as_str());
            assert_eq!(drop.map(|a| &a["name"]), Some(name), "{say}: {id}'s name");
        }
    }
    if let Some(logos) = expected["logos"].as_array() {
        for id in logos {
            let drop = agents.iter().find(|a| &a["id"] == id);
            let logo = drop.and_then(|a| a["logo"].as_str()).unwrap_or_default();
            assert!(logo.ends_with(".png"), "{say}: {id} shows its logo");
        }
    }
    if let Some(more) = expected["moreColumns"].as_u64() {
        assert_eq!(
            last["trace"]["more"].as_u64().unwrap_or(0),
            more,
            "{say}: the columns not shown"
        );
    }
    if let Some(offered) = expected["stopAllOffered"].as_bool() {
        let busy = last["agents"].as_array().expect("agents").iter().any(|a| {
            ["working", "paused", "needsYou"].contains(&a["status"].as_str().unwrap_or_default())
        });
        assert_eq!(busy, offered, "{say}: Stop all agents is offered");
    }
    Step {
        say,
        gesture,
        intent,
        rosters,
        bars,
        window: Value::Null,
        settled: false,
        expect: expected,
    }
}

/// `expected`, with how many more runs the trace tree counts than it shows ("+N more running").
fn with_more_columns(mut expected: Value, more: u64) -> Value {
    expected["moreColumns"] = json!(more);
    expected
}

/// `checked`, for a step whose runs end in no fixed order (Stop all cancels them together): only
/// the roster after the last one is kept, so the transcript is the same on every run.
fn checked_settled(
    core: &Core,
    say: &'static str,
    gesture: Value,
    intent: Value,
    expected: Value,
) -> Step {
    {
        let mut sent = core.sent.lock().expect("sent");
        let last = sent.pop();
        sent.clear();
        sent.extend(last);
    }
    let mut step = checked(core, say, gesture, intent, expected);
    step.settled = true;
    step
}

/// `expected`, with the name each drop shows (`agent`, `name`).
fn with_names(mut expected: Value, names: &[(&str, &str)]) -> Value {
    let map: serde_json::Map<String, Value> = names
        .iter()
        .map(|(id, name)| ((*id).to_owned(), json!(name)))
        .collect();
    expected["names"] = Value::Object(map);
    expected
}

/// `expected`, with the drops that show their provider's logo.
fn with_logos(mut expected: Value, agents: &[&str]) -> Value {
    expected["logos"] = json!(agents);
    expected
}

/// `expected`, with whether the approval box offers Allow for this run.
fn with_for_run(mut expected: Value, offered: bool) -> Value {
    expected["approval"]["forRun"] = json!(offered);
    expected
}

/// `expected`, with whether the corral's menu offers Stop all agents.
fn with_stop_all(mut expected: Value, offered: bool) -> Value {
    expected["stopAllOffered"] = json!(offered);
    expected
}

/// A main-window step: `checked`, with the command it called. `gesture` is what the user does in
/// the window, which the Vitest journey test performs.
fn in_window(core: &Core, say: &'static str, gesture: Value, call: Value, expected: Value) -> Step {
    let mut window = call;
    window["gesture"] = gesture;
    let mut step = checked(core, say, Value::Null, Value::Null, expected);
    step.window = window;
    step
}

/// A stand-in agent beyond today's catalog: the corral treats every agent the same.
fn stand_in(id: &str, name: &str) -> AgentCard {
    AgentCard {
        id: AgentId::from(id),
        name: name.to_owned(),
        monogram: name.chars().take(1).collect(),
    }
}

fn transcript_path(journey: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../native/CorralNative/Fixtures/journeys")
        .join(format!("{journey}.json"))
}

fn transcript(journey: &str, title: &str, steps: &[Step]) -> String {
    let value = json!({
        "journey": journey,
        "title": title,
        "steps": steps.iter().map(Step::to_json).collect::<Vec<_>>(),
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("transcript")
    )
}

/// Fails when the checked-in transcript isn't what the core produced (or writes it).
fn matches_checked_in(journey: &str, played: &str) {
    let path = transcript_path(journey);
    if std::env::var_os("CORRAL_WRITE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().expect("folder")).expect("journeys folder");
        std::fs::write(&path, played).expect("write the transcript");
    }
    let committed = std::fs::read_to_string(&path).expect("read the transcript");
    assert!(
        committed == played,
        "{} is out of date with the core. Regenerate it:\n\n    \
         CORRAL_WRITE_FIXTURES=1 cargo test -p corral-desktop --test journeys\n",
        path.display()
    );
}

/// Plays a journey twice (the same each time) and checks it against its transcript.
async fn check<F, Fut>(journey: &str, play: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let played = play().await;
    assert_eq!(play().await, played, "{journey} is the same on every run");
    matches_checked_in(journey, &played);
}

// ---- J9: the corral morphs ---------------------------------------------------------------

const J9_TASK: &str = "Summarize my inbox";

/// J9 (`docs/04-test-plan.md` §6): three agents make a triangle; deploying one keeps the
/// triangle and turns its ring; a fourth agent makes a square; removing one makes a triangle
/// again; the working one finishes (its column folds away) and is sent back, still a triangle.
async fn play_j9() -> String {
    let script = FakeScript::default()
        .say("Three emails need you today: the lease renewal, Sam's itinerary and a refund.")
        .wait(Duration::from_secs(40))
        .usage(1_200, 80, 2_000);
    let core = Core::new(
        Arc::new(FakeDriver::new(FakeScript::default()).with_prompt(J9_TASK, script)),
        Arc::new(CheckpointDesk::default()),
        &[Provider::Xai, Provider::OpenAi, Provider::Anthropic],
    );
    let none = Value::Null;
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Grok, GPT and Claude",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[
                ("grok", "available"),
                ("gpt", "available"),
                ("claude", "available"),
            ],
            &[],
        ),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "deployTask", "id": "grok", "prompt": J9_TASK}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Give Grok a task in its card: still a triangle, its ring turns, its column rises",
        json!({"kind": "task", "agent": "grok", "text": J9_TASK}),
        intent,
        expect(
            "triangle",
            &[
                ("grok", "working"),
                ("gpt", "available"),
                ("claude", "available"),
            ],
            &["grok"],
        ),
    ));

    core.set_agents(&[
        Provider::Xai,
        Provider::OpenAi,
        Provider::Anthropic,
        Provider::Meta,
    ]);
    steps.push(checked(
        &core,
        "Connect a Meta key: Muse Spark joins and the corral becomes a square",
        none.clone(),
        none.clone(),
        expect(
            "square",
            &[
                ("grok", "working"),
                ("gpt", "available"),
                ("claude", "available"),
                ("muse-spark", "available"),
            ],
            &["grok"],
        ),
    ));

    core.set_agents(&[Provider::Xai, Provider::Anthropic, Provider::Meta]);
    steps.push(checked(
        &core,
        "Sign out of ChatGPT: GPT leaves and the corral is a triangle again",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[
                ("grok", "working"),
                ("claude", "available"),
                ("muse-spark", "available"),
            ],
            &["grok"],
        ),
    ));

    wait(60).await;
    steps.push(checked(
        &core,
        "Grok finishes: its drop shows it's done, and its column folds away",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[
                ("grok", "done"),
                ("claude", "available"),
                ("muse-spark", "available"),
            ],
            &[],
        ),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "sendBack", "id": "grok"}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Send Grok back from its card: still a triangle, and Grok is ready for a task",
        json!({"kind": "press", "agent": "grok", "button": "sendBack"}),
        intent,
        expect(
            "triangle",
            &[
                ("grok", "available"),
                ("claude", "available"),
                ("muse-spark", "available"),
            ],
            &[],
        ),
    ));

    transcript("J9", "Corral morph", &steps)
}

#[tokio::test(start_paused = true)]
async fn j9_the_corral_morphs_as_agents_come_and_go() {
    check("J9", play_j9).await;
}

// ---- J2: three agents at once --------------------------------------------------------------

const J2_TASK: &str = "Plan my week";

/// Each agent's own run of the same task, finishing at its own pace.
fn j2_driver() -> Arc<dyn RunDriver> {
    let run = |seconds: u64, plan: &str| {
        Arc::new(
            FakeDriver::new(FakeScript::default()).with_prompt(
                J2_TASK,
                FakeScript::default()
                    .say(plan.to_owned())
                    .wait(Duration::from_secs(seconds))
                    .usage(1_000, 200, 2_000),
            ),
        ) as Arc<dyn RunDriver>
    };
    let mut drivers: HashMap<AgentId, Arc<dyn RunDriver>> = HashMap::new();
    drivers.insert(
        agent_for(Provider::Xai).id,
        run(10, "Monday: the Q4 draft."),
    );
    drivers.insert(
        agent_for(Provider::OpenAi).id,
        run(20, "Tuesday: errands and the dentist."),
    );
    drivers.insert(
        agent_for(Provider::Anthropic).id,
        run(30, "Friday: keep it free."),
    );
    Arc::new(AgentRouter::new(drivers))
}

/// J2 (`docs/04-test-plan.md` §6): ⌘-click three drops, type a task in the prompt bar and press
/// "Deploy to 3"; each agent's column rises and then folds away as that agent finishes, on its
/// own.
async fn play_j2() -> String {
    let core = Core::new(
        j2_driver(),
        Arc::new(CheckpointDesk::default()),
        &[Provider::Xai, Provider::OpenAi, Provider::Anthropic],
    );
    let none = Value::Null;
    let all_ready = [
        ("grok", "available"),
        ("gpt", "available"),
        ("claude", "available"),
    ];
    let selection = json!(["grok", "gpt", "claude"]);
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Grok, GPT and Claude",
        none.clone(),
        none.clone(),
        expect("triangle", &all_ready, &[]),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarEdited", "seq": 1, "text": "", "cursor": 0, "selection": selection}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "⌘-click Claude, Grok and GPT, then open the prompt bar: it's for the three",
        json!({"kind": "openBar", "select": ["claude", "grok", "gpt"]}),
        intent,
        with_bar(
            expect("triangle", &all_ready, &[]),
            &["3 selected"],
            "Deploy to 3",
            None,
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarEdited", "seq": 2, "text": J2_TASK, "cursor": J2_TASK.len(),
        "selection": selection}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Type the task",
        json!({"kind": "typeInBar", "text": J2_TASK}),
        intent,
        with_bar(
            expect("triangle", &all_ready, &[]),
            &["3 selected"],
            "Deploy to 3",
            None,
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarSend", "seq": 3, "text": J2_TASK, "selection": selection, "all": false}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Press Deploy to 3: all three work, each with its own column",
        json!({"kind": "sendBar"}),
        intent,
        with_bar(
            expect(
                "triangle",
                &[
                    ("grok", "working"),
                    ("gpt", "working"),
                    ("claude", "working"),
                ],
                &["grok", "gpt", "claude"],
            ),
            &["3 selected"],
            "Deploy to 3",
            Some("sent"),
        ),
    ));

    wait(10).await;
    steps.push(checked(
        &core,
        "Grok finishes first: only its column folds away",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[("grok", "done"), ("gpt", "working"), ("claude", "working")],
            &["gpt", "claude"],
        ),
    ));

    wait(10).await;
    steps.push(checked(
        &core,
        "Then GPT",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[("grok", "done"), ("gpt", "done"), ("claude", "working")],
            &["claude"],
        ),
    ));

    wait(10).await;
    steps.push(checked(
        &core,
        "Then Claude: every column has folded away",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[("grok", "done"), ("gpt", "done"), ("claude", "done")],
            &[],
        ),
    ));

    transcript("J2", "Three agents at once", &steps)
}

#[tokio::test(start_paused = true)]
async fn j2_three_agents_deployed_at_once_update_on_their_own() {
    check("J2", play_j2).await;
}

// ---- J3: every way to answer a checkpoint ----------------------------------------------------

const J3_TASK: &str = "Tidy my notes";

/// The corral of J3 while Claude waits on you.
fn j3_waiting() -> Value {
    with_column_status(
        expect(
            "dumbbell",
            &[("grok", "available"), ("claude", "needsYou")],
            &["claude"],
        ),
        &[("claude", "needsYou")],
    )
}

/// J3 (`docs/04-test-plan.md` §6): one run asks four times. Approve the first save; deny the
/// email with a note, and the agent carries on; edit the backup's path and approve the edited
/// call; allow saves for the rest of the run, and the next save doesn't ask.
async fn play_j3() -> String {
    let desk = Arc::new(CheckpointDesk::default());
    let script = FakeScript::default()
        .say("Saving a tidier copy.")
        .tool(
            "fs.write",
            json!({"path": "notes.md", "text": "This week: call the dentist, draft the Q4 plan"}),
        )
        .say(" Emailing it to Sam.")
        .tool(
            "email.send",
            json!({"to": "sam@example.test", "body": "My notes, tidied"}),
        )
        .say(" OK, I won't email anyone. Saving a backup.")
        .tool(
            "fs.write",
            json!({"path": "notes-backup.md", "text": "This week: call the dentist"}),
        )
        .say(" Writing the to-do list.")
        .tool(
            "fs.write",
            json!({"path": "todo.md", "text": "Call the dentist"}),
        )
        .say(" And the done list.")
        .tool(
            "fs.write",
            json!({"path": "done.md", "text": "Booked flights"}),
        )
        .say(" All tidy.")
        .usage(3_000, 600, 9_000);
    let core = Core::new(
        Arc::new(
            FakeDriver::new(FakeScript::default())
                .with_prompt(J3_TASK, script)
                .with_tools(tools(&desk, "j3")),
        ),
        desk,
        &[Provider::Xai, Provider::Anthropic],
    );
    let none = Value::Null;
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Grok and Claude",
        none.clone(),
        none.clone(),
        expect(
            "dumbbell",
            &[("grok", "available"), ("claude", "available")],
            &[],
        ),
    ));

    let intent =
        json!({"v": 1, "intent": {"kind": "deployTask", "id": "claude", "prompt": J3_TASK}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("claude");
    steps.push(checked(
        &core,
        "Give Claude a task: it asks before saving a file",
        json!({"kind": "task", "agent": "claude", "text": J3_TASK}),
        intent,
        with_approval(
            j3_waiting(),
            "claude",
            "write.local",
            "normal",
            &["notes.md"],
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "approveCheckpoint", "id": "claude", "checkpoint": cp, "scope": "once",
        "how": "click"}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("claude");
    steps.push(checked(
        &core,
        "Approve: it saves, then asks before emailing Sam",
        json!({"kind": "press", "agent": "claude", "button": "approve"}),
        intent,
        with_approval(
            j3_waiting(),
            "claude",
            "comms.send",
            "high",
            &["sam@example.test", "My notes, tidied"],
        ),
    ));

    let note = "Don't email anyone yet.";
    let intent = json!({"v": 1, "intent": {
        "kind": "denyCheckpoint", "id": "claude", "checkpoint": cp, "note": note}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("claude");
    steps.push(checked(
        &core,
        "Deny with a note in the card's field: it carries on, and asks before saving a backup",
        json!({"kind": "note", "agent": "claude", "text": note}),
        intent,
        with_approval(
            j3_waiting(),
            "claude",
            "write.local",
            "normal",
            &["notes-backup.md"],
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "editCheckpoint", "id": "claude", "checkpoint": cp,
        "changes": {"path": "archive/notes-backup.md"}, "how": "click"}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("claude");
    steps.push(checked(
        &core,
        "Edit the backup's path and approve the edited call: then it asks before the to-do list",
        json!({"kind": "edit", "agent": "claude", "changes": {"path": "archive/notes-backup.md"}}),
        intent,
        with_approval(
            j3_waiting(),
            "claude",
            "write.local",
            "normal",
            &["todo.md"],
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "approveCheckpoint", "id": "claude", "checkpoint": cp, "scope": "forRun",
        "how": "click"}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Allow for this run: the to-do list and the done list save without asking, and it's done",
        json!({"kind": "press", "agent": "claude", "button": "allowForRun"}),
        intent,
        expect(
            "dumbbell",
            &[("grok", "available"), ("claude", "done")],
            &[],
        ),
    ));

    transcript("J3", "Answering checkpoints", &steps)
}

#[tokio::test(start_paused = true)]
async fn j3_a_checkpoint_is_approved_denied_edited_and_allowed_for_the_run() {
    check("J3", play_j3).await;
}

// ---- J11: approving from the hover card ----------------------------------------------------

const J11_TASK: &str = "Send the itinerary to Sam";

/// The corral of J11 while Grok waits on you: its drop pulses and its wire turns maroon.
fn j11_waiting() -> Value {
    with_column_status(
        expect(
            "triangle",
            &[
                ("grok", "needsYou"),
                ("gpt", "available"),
                ("claude", "available"),
            ],
            &["grok"],
        ),
        &[("grok", "needsYou")],
    )
}

/// J11 (`docs/04-test-plan.md` §6): an agent asks to send an email; its drop and its column wait
/// on you; the card shows where it goes and what it says; Deny with a note, and the agent tries
/// another way; it asks again, and Approve on the pinned card lets the column go on.
async fn play_j11() -> String {
    let desk = Arc::new(CheckpointDesk::default());
    let script = FakeScript::default()
        .say("Sending Sam the itinerary.")
        .tool(
            "email.send",
            json!({"to": "sam@example.test", "body": "Flights: SFO to LIS, Dec 12"}),
        )
        .say(" OK. I'll ask Sam's assistant to confirm the dates first.")
        .tool(
            "email.send",
            json!({"to": "alex@example.test", "body": "Can you confirm Sam's dates?"}),
        )
        .say(" Sent. Waiting for Alex.")
        .wait(Duration::from_secs(30))
        .say(" Alex confirmed Dec 12.")
        .usage(1_500, 300, 4_000);
    let core = Core::new(
        Arc::new(
            FakeDriver::new(FakeScript::default())
                .with_prompt(J11_TASK, script)
                .with_tools(tools(&desk, "j11")),
        ),
        desk,
        &[Provider::Xai, Provider::OpenAi, Provider::Anthropic],
    );
    let none = Value::Null;
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Grok, GPT and Claude",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[
                ("grok", "available"),
                ("gpt", "available"),
                ("claude", "available"),
            ],
            &[],
        ),
    ));

    let intent =
        json!({"v": 1, "intent": {"kind": "deployTask", "id": "grok", "prompt": J11_TASK}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("grok");
    steps.push(checked(
        &core,
        "Grok asks to send an email: its drop and its wire wait on you, and the card shows the \
         destination and the message",
        json!({"kind": "task", "agent": "grok", "text": J11_TASK}),
        intent,
        with_approval(
            j11_waiting(),
            "grok",
            "comms.send",
            "high",
            &["sam@example.test", "Flights: SFO to LIS, Dec 12"],
        ),
    ));

    let note = "Wait until I confirm the dates.";
    let intent = json!({"v": 1, "intent": {
        "kind": "denyCheckpoint", "id": "grok", "checkpoint": cp, "note": note}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("grok");
    steps.push(checked(
        &core,
        "Deny with a note: Grok carries on and asks to email Alex instead",
        json!({"kind": "note", "agent": "grok", "text": note}),
        intent,
        with_approval(
            j11_waiting(),
            "grok",
            "comms.send",
            "high",
            &["alex@example.test", "Can you confirm Sam's dates?"],
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "approveCheckpoint", "id": "grok", "checkpoint": cp, "scope": "once",
        "how": "click"}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Pin the card and Approve: the email goes, and Grok's column goes on",
        json!({"kind": "press", "agent": "grok", "button": "approve", "pinned": true}),
        intent,
        with_column_status(
            expect(
                "triangle",
                &[
                    ("grok", "working"),
                    ("gpt", "available"),
                    ("claude", "available"),
                ],
                &["grok"],
            ),
            &[("grok", "working")],
        ),
    ));

    wait(40).await;
    steps.push(checked(
        &core,
        "Grok finishes",
        none.clone(),
        none.clone(),
        expect(
            "triangle",
            &[
                ("grok", "done"),
                ("gpt", "available"),
                ("claude", "available"),
            ],
            &[],
        ),
    ));

    transcript("J11", "Hover approval", &steps)
}

#[tokio::test(start_paused = true)]
async fn j11_a_checkpoint_is_denied_then_approved_from_the_hover_card() {
    check("J11", play_j11).await;
}

// ---- J6: Stop all, with five runs ----------------------------------------------------------

const J6_TASK: &str = "Research the trip";

/// Today's catalog and one stand-in: five agents, so five runs at once.
fn j6_agents() -> Vec<AgentCard> {
    let mut agents = cards(&[
        Provider::Xai,
        Provider::OpenAi,
        Provider::Anthropic,
        Provider::Meta,
    ]);
    agents.push(stand_in("scout", "Scout"));
    agents
}

/// J6 (`docs/04-test-plan.md` §6): one task to every agent from the prompt bar (⌘↩), then
/// Stop all agents from the corral's right-click menu: every run stops, every column folds away,
/// the menu stops offering it, and each agent can take a new task.
async fn play_j6() -> String {
    let script = FakeScript::default()
        .say("Looking at flights and hotels.")
        .wait(Duration::from_secs(300))
        .say(" Found three options.");
    let core = Core::with_cards(
        // Every task takes a while, so the runs are still going when Stop all is pressed.
        Arc::new(FakeDriver::new(script)),
        Arc::new(CheckpointDesk::default()),
        j6_agents(),
    );
    let none = Value::Null;
    let ids = ["grok", "gpt", "claude", "muse-spark", "scout"];
    let all = |status: &'static str| ids.map(|id| (id, status));
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with five agents",
        none.clone(),
        none.clone(),
        with_stop_all(expect("pentagon", &all("available"), &[]), false),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarEdited", "seq": 1, "text": "", "cursor": 0, "selection": []}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Open the prompt bar with nothing selected",
        json!({"kind": "openBar", "select": []}),
        intent,
        expect("pentagon", &all("available"), &[]),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarEdited", "seq": 2, "text": J6_TASK, "cursor": J6_TASK.len(),
        "selection": []}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Type the task",
        json!({"kind": "typeInBar", "text": J6_TASK}),
        intent,
        expect("pentagon", &all("available"), &[]),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "promptBarSend", "seq": 3, "text": J6_TASK, "selection": [], "all": true}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "⌘↩ sends it to every agent: five runs, three columns and \"+2 more running\"",
        json!({"kind": "sendBar", "all": true}),
        intent,
        with_stop_all(
            with_more_columns(expect("pentagon", &all("working"), &ids[..3]), 2),
            true,
        ),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "stopAll"}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked_settled(
        &core,
        "Right-click the corral and choose Stop all agents: every run stops and its column folds \
         away",
        json!({"kind": "stopAll"}),
        intent,
        with_stop_all(expect("pentagon", &all("available"), &[]), false),
    ));

    let task = "Book the hotel";
    let intent = json!({"v": 1, "intent": {"kind": "deployTask", "id": "claude", "prompt": task}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Claude takes a new task",
        json!({"kind": "task", "agent": "claude", "text": task}),
        intent,
        with_stop_all(
            expect(
                "pentagon",
                &[
                    ("grok", "available"),
                    ("gpt", "available"),
                    ("claude", "working"),
                    ("muse-spark", "available"),
                    ("scout", "available"),
                ],
                &["claude"],
            ),
            true,
        ),
    ));

    transcript("J6", "Stop all", &steps)
}

#[tokio::test(start_paused = true)]
async fn j6_stop_all_stops_five_runs_and_each_agent_can_go_again() {
    check("J6", play_j6).await;
}

// ---- J10: the usage order and the shown set ------------------------------------------------

const J10_TASK: &str = "Check the forecast";

/// Today's catalog and four stand-ins: a full corral of eight.
fn j10_agents() -> Vec<AgentCard> {
    let mut agents = cards(&[
        Provider::Xai,
        Provider::OpenAi,
        Provider::Anthropic,
        Provider::Meta,
    ]);
    for (id, name) in [
        ("scout", "Scout"),
        ("atlas", "Atlas"),
        ("nova", "Nova"),
        ("echo", "Echo"),
    ] {
        agents.push(stand_in(id, name));
    }
    agents
}

/// The deploys of the last 90 days: ten days ago, Grok most and GPT fourth.
fn j10_history() -> Vec<Deploy> {
    let ten_days_ago = START_MS - 10 * 24 * 3_600_000;
    [
        ("grok", 6),
        ("claude", 5),
        ("scout", 4),
        ("gpt", 3),
        ("atlas", 2),
        ("nova", 1),
    ]
    .into_iter()
    .flat_map(|(agent, times)| {
        (0..times).map(move |n| Deploy {
            agent: AgentId::from(agent),
            ts_ms: ten_days_ago + n * 60_000,
        })
    })
    .collect()
}

/// J10 (`docs/04-test-plan.md` §6): deploy the rank-4 agent until it outranks rank 1; the
/// corral keeps its order while you're in it, and once you leave it GPT glides to the top-left.
/// A 9th agent keeps the corral at eight and asks which show; untick one (a heptagon) and tick
/// the new one, and the corral is an octagon of the chosen eight.
async fn play_j10() -> String {
    let script = FakeScript::default().say("Sunny, 18°C.");
    let core = Core::with_cards(
        Arc::new(FakeDriver::new(FakeScript::default()).with_prompt(J10_TASK, script)),
        Arc::new(CheckpointDesk::default()),
        j10_agents(),
    );
    core.seed(j10_history());
    let none = Value::Null;
    let mut steps = Vec::new();
    let by_use = [
        "grok",
        "claude",
        "scout",
        "gpt",
        "atlas",
        "nova",
        "muse-spark",
        "echo",
    ];
    let ready = |order: &[&'static str]| -> Vec<(&'static str, &'static str)> {
        order.iter().map(|id| (*id, "available")).collect()
    };
    let gpt_done = |order: &[&'static str]| -> Vec<(&'static str, &'static str)> {
        order
            .iter()
            .map(|id| (*id, if *id == "gpt" { "done" } else { "available" }))
            .collect()
    };

    steps.push(checked(
        &core,
        "The corral opens with eight agents, most used first: GPT is fourth",
        none.clone(),
        none.clone(),
        expect("octagon", &ready(&by_use), &[]),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "corralIdle", "idle": false}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Point at GPT: its card opens, and the corral isn't idle",
        json!({"kind": "pointAt", "agent": "gpt"}),
        intent,
        expect("octagon", &ready(&by_use), &[]),
    ));

    for say in [
        "Give GPT a task from its card: it runs and finishes, and the order stays",
        "And again",
        "And a third time: GPT now outranks Grok, but the drops wait while you're in the corral",
    ] {
        let intent =
            json!({"v": 1, "intent": {"kind": "deployTask", "id": "gpt", "prompt": J10_TASK}});
        core.intent(&intent).await;
        wait(1).await;
        steps.push(checked(
            &core,
            say,
            json!({"kind": "task", "agent": "gpt", "text": J10_TASK}),
            intent,
            expect("octagon", &gpt_done(&by_use), &[]),
        ));
    }

    let glided = [
        "gpt",
        "grok",
        "claude",
        "scout",
        "atlas",
        "nova",
        "muse-spark",
        "echo",
    ];
    let intent = json!({"v": 1, "intent": {"kind": "corralIdle", "idle": true}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Leave the corral: once it's idle, GPT glides to the top-left",
        json!({"kind": "leaveCorral"}),
        intent,
        expect("octagon", &gpt_done(&glided), &[]),
    ));

    let mut nine = j10_agents();
    nine.push(stand_in("pilot", "Pilot"));
    core.set_cards(nine);
    steps.push(checked(
        &core,
        "A 9th agent connects: the corral stays an octagon, and Pilot waits to be chosen",
        none.clone(),
        none.clone(),
        expect("octagon", &gpt_done(&glided), &[]),
    ));

    let call = core.window_settings();
    steps.push(in_window(
        &core,
        "Settings → Corral asks which 8 show, with Pilot unticked",
        json!({"kind": "openCorralSettings"}),
        call,
        expect("octagon", &gpt_done(&glided), &[]),
    ));

    let call = core.window_toggle("scout");
    let without_scout = [
        "gpt",
        "grok",
        "claude",
        "atlas",
        "nova",
        "muse-spark",
        "echo",
    ];
    steps.push(in_window(
        &core,
        "Untick Scout: the corral is a heptagon",
        json!({"kind": "untick", "name": "Scout"}),
        call,
        expect("heptagon", &gpt_done(&without_scout), &[]),
    ));

    let call = core.window_toggle("pilot");
    let chosen = [
        "gpt",
        "grok",
        "claude",
        "atlas",
        "nova",
        "muse-spark",
        "echo",
        "pilot",
    ];
    steps.push(in_window(
        &core,
        "Tick Pilot: an octagon of the chosen eight",
        json!({"kind": "tick", "name": "Pilot"}),
        call,
        expect("octagon", &gpt_done(&chosen), &[]),
    ));

    transcript("J10", "Usage order and shown set", &steps)
}

#[tokio::test(start_paused = true)]
async fn j10_the_most_used_agent_glides_to_the_top_once_idle_and_eight_are_chosen() {
    check("J10", play_j10).await;
}

// ---- The rest of the app, for J1 -----------------------------------------------------------

/// A made-up xAI key, typed into the key field. It never reaches a transcript: the window steps
/// record the provider and the status the core answers, which shows only its last 4.
const J1_KEY: &str = "xai-journeyNotARealKey7Qm2Lp9Rt4Wx"; // gitleaks:allow (made-up test key)

/// The models check, accepting the key (the app's asks the provider; no network here).
struct Accepting;

impl ModelsProbe for Accepting {
    fn get<'a>(
        &'a self,
        _url: &'a str,
        _headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        Box::pin(async { Ok(200) })
    }
}

/// The provider's site, sending its logo: a small PNG.
struct FakeLogos;

impl LogoFetcher for FakeLogos {
    fn get<'a>(&'a self, _source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&64_u32.to_be_bytes());
        png.extend_from_slice(&64_u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        png.extend_from_slice(&[0; 32]);
        Box::pin(async move { Ok(png) })
    }
}

/// The ChatGPT sign-in, never asked: J1 doesn't sign in.
struct NoNetwork;

impl FormTransport for NoNetwork {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        _form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move { Err(TransportError::Exhausted(format!("no network: {url}"))) })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move { Err(TransportError::Exhausted(format!("no network: {url}"))) })
    }
}

/// What the transcripts call the user's home folder, as the bridge fixtures do.
const HOME: &str = "/Users/you";

/// The app around the core: the Keychain (in memory), ChatGPT signed out, the agents' settings and
/// names, and the logos folder, all under a scratch home folder that's removed afterwards. Paths
/// under it are recorded as under [`HOME`], so a transcript is the same on every machine.
struct AppSide {
    home: PathBuf,
    keys: Arc<MemoryStore>,
    plan: ChatGptPlan,
    names: AgentNames,
    editor: AgentEditor,
    logos: LogoStore,
    /// The editor told the corral to refresh (its `on_change`).
    told: Arc<AtomicU64>,
    /// The event store the core logs to, as the app's, and the trace viewer reading it.
    store: Arc<Store>,
    traces: Traces,
}

impl AppSide {
    fn new(journey: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "corral-journey-{journey}-{}-{}",
            std::process::id(),
            HOMES.fetch_add(1, Ordering::SeqCst)
        ));
        let support = home.join("Library/Application Support/Corral");
        std::fs::create_dir_all(&support).expect("a scratch home");
        let names = AgentNames::default();
        let editor = AgentEditor::open(
            AgentSettings::new(support.join("agents")),
            names.clone(),
            Some(home.clone()),
        );
        let store = Arc::new(Store::open(&support.join("events.db")).expect("the event store"));
        let traces = Traces::named(names.clone());
        traces.attach(Arc::clone(&store));
        let told = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&told);
        editor.on_change(Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        Self {
            logos: LogoStore::new(support.join("logos")),
            home,
            keys: Arc::new(MemoryStore::default()),
            plan: ChatGptPlan::new(
                Arc::new(MemoryPlanStore::default()),
                Arc::new(NoNetwork),
                Arc::new(PausedClock(tokio::time::Instant::now())),
            ),
            names,
            editor,
            told,
            store,
            traces,
        }
    }

    fn connected(&self) -> Vec<Provider> {
        keys::connected(self.keys.as_ref(), &self.plan)
    }

    fn agents(&self) -> CorralAgents {
        corral_for(&self.connected(), &LinkedSettings::default(), &self.names)
    }

    /// `text`, with the scratch home recorded as [`HOME`].
    fn at_home(&self, text: &str) -> String {
        text.replace(&*self.home.to_string_lossy(), HOME)
    }
}

impl Drop for AppSide {
    fn drop(&mut self) {
        // Cleanup only; a leftover scratch folder in the temp folder harms nothing.
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

static HOMES: AtomicU64 = AtomicU64::new(0);

/// A main-window command's answer, as JSON.
fn answer(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("an answer")
}

// ---- J1: first launch to a finished task -----------------------------------------------------

const J1_TASK: &str = "Draft my week's plan";

/// J1 (`docs/04-test-plan.md` §6): first launch with no keys and an empty corral; add an xAI
/// key in Settings, and Grok drops in and shows its logo; name it Ada in the agent editor; give
/// it a task and watch its column and card; it finishes, and is sent back.
async fn play_j1() -> String {
    let script = FakeScript::default()
        .say("Monday: the Q4 draft. Tuesday: errands.")
        .wait(Duration::from_secs(20))
        .say(" Friday stays free.")
        .usage(1_000, 300, 3_000);
    let app = AppSide::new("j1");
    let core = Core::with_app(
        Arc::new(FakeDriver::new(FakeScript::default()).with_prompt(J1_TASK, script)),
        Arc::new(CheckpointDesk::default()),
        app,
    );
    let app = core.app.as_ref().expect("the app");
    let none = Value::Null;
    let mut steps = Vec::new();

    let providers = ["xai", "anthropic", "openai", "meta"];
    let statuses: Vec<Value> = [
        Provider::Xai,
        Provider::Anthropic,
        Provider::OpenAi,
        Provider::Meta,
    ]
    .into_iter()
    .map(|p| answer(keys::key_status(app.keys.as_ref(), p).expect("a status")))
    .collect();
    steps.push(in_window(
        &core,
        "First launch: no keys, so the corral is empty, and Settings shows no providers",
        json!({"kind": "openKeys"}),
        json!({"command": "provider_key_status", "args": {"providers": providers},
               "answer": statuses}),
        expect("empty", &[], &[]),
    ));

    let identified = keys::identify_key(J1_KEY);
    let connected = keys::connect_key(app.keys.as_ref(), &Accepting, Provider::Xai, J1_KEY.into())
        .await
        .expect("connected");
    core.refresh().await;
    steps.push(in_window(
        &core,
        "Paste an xAI key and Connect: Grok drops in, with its logo",
        json!({"kind": "pasteKey", "looksLike": identified}),
        json!({"command": "connect_provider_key", "args": {"provider": "xai"},
               "answer": answer(connected)}),
        with_logos(
            with_names(
                expect("circle", &[("grok", "available")], &[]),
                &[("grok", "Grok")],
            ),
            &["grok"],
        ),
    ));

    let views = app.editor.views();
    steps.push(in_window(
        &core,
        "Settings → Agents shows Grok",
        json!({"kind": "openAgents"}),
        json!({"command": "agent_editor", "args": {}, "answer": answer(&views)}),
        expect("circle", &[("grok", "available")], &[]),
    ));

    let grok = views
        .iter()
        .find(|v| v.agent == "grok")
        .expect("Grok's settings");
    let edit = json!({"name": "Ada", "model": grok.model, "monthlyCapMicroUsd": grok.monthly_cap_micro_usd});
    let parsed: AgentEdit = serde_json::from_value(edit.clone()).expect("the command takes it");
    let saved = app.editor.save("grok", parsed).expect("saved");
    assert_eq!(
        app.told.load(Ordering::SeqCst),
        1,
        "the editor tells the corral"
    );
    core.refresh().await;
    steps.push(in_window(
        &core,
        "Name it Ada and Save: its drop is Ada now",
        json!({"kind": "rename", "agent": "grok", "name": "Ada"}),
        json!({"command": "save_agent", "args": {"agent": "grok", "edit": edit},
               "answer": answer(saved)}),
        with_logos(
            with_names(
                expect("circle", &[("grok", "available")], &[]),
                &[("grok", "Ada")],
            ),
            &["grok"],
        ),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "deployTask", "id": "grok", "prompt": J1_TASK}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Give Ada a task in its card: its ring turns and its column rises",
        json!({"kind": "task", "agent": "grok", "text": J1_TASK}),
        intent,
        with_column_status(
            expect("circle", &[("grok", "working")], &["grok"]),
            &[("grok", "working")],
        ),
    ));

    wait(30).await;
    steps.push(checked(
        &core,
        "Ada finishes: its card offers Send back and a new task",
        none.clone(),
        none.clone(),
        expect("circle", &[("grok", "done")], &[]),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "sendBack", "id": "grok"}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Send Ada back: it's ready for a task",
        json!({"kind": "press", "agent": "grok", "button": "sendBack"}),
        intent,
        with_names(
            expect("circle", &[("grok", "available")], &[]),
            &[("grok", "Ada")],
        ),
    ));

    let played = transcript("J1", "First launch", &steps);
    app.at_home(&played)
}

#[tokio::test(start_paused = true)]
async fn j1_first_launch_to_a_finished_task() {
    check("J1", play_j1).await;
}

// ---- J5: the soft limit ----------------------------------------------------------------------

const J5_TASK: &str = "Summarize the quarterly reports";

/// J5 (`docs/04-test-plan.md` §6): a run reaches the app's soft limit ($0.10): nothing more is
/// sent, and its card asks; Approve raises the limit to the run's cap, and the run goes on.
async fn play_j5() -> String {
    let desk = Arc::new(CheckpointDesk::default());
    let script = FakeScript::default()
        .say("Reading the Q1 and Q2 reports.")
        // The first turn bills $0.104: past the soft limit, so the next request waits.
        .usage(40_000, 2_000, 104_000)
        .request()
        .say(" Q3 and Q4 next.")
        .usage(20_000, 1_000, 40_000)
        .say(" Done: revenue grew each quarter.");
    let app = AppSide::new("j5");
    app.keys
        .set(
            Provider::Anthropic,
            &corral_secrets::Secret::new(J5_KEY).expect("a key"),
        )
        .expect("stored");
    let core = Core::with_app(
        Arc::new(
            FakeDriver::new(FakeScript::default())
                .with_prompt(J5_TASK, script)
                // Priced, so each request's worst case counts against the limit.
                .priced(Price::per_million_tokens(2_000_000, 6_000_000))
                .with_tools(tools(&desk, "j5")),
        ),
        desk,
        app,
    );
    let none = Value::Null;
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Claude",
        none.clone(),
        none.clone(),
        expect("circle", &[("claude", "available")], &[]),
    ));

    let intent =
        json!({"v": 1, "intent": {"kind": "deployTask", "id": "claude", "prompt": J5_TASK}});
    core.intent(&intent).await;
    wait(1).await;
    let cp = core.waiting("claude");
    steps.push(checked(
        &core,
        "Give Claude a task: it reaches the $0.10 soft limit, and its card asks before spending more",
        json!({"kind": "task", "agent": "claude", "text": J5_TASK}),
        intent,
        with_for_run(
            with_approval(
                with_column_status(
                    expect("circle", &[("claude", "needsYou")], &["claude"]),
                    &[("claude", "needsYou")],
                ),
                "claude",
                "budget.threshold",
                "normal",
                &["soft limit is $0.10", "up to its $0.25 cap"],
            ),
            false,
        ),
    ));

    let intent = json!({"v": 1, "intent": {
        "kind": "approveCheckpoint", "id": "claude", "checkpoint": cp, "scope": "once",
        "how": "click"}});
    core.intent(&intent).await;
    wait(1).await;
    steps.push(checked(
        &core,
        "Approve: the limit is raised to the run's cap, and Claude finishes",
        json!({"kind": "press", "agent": "claude", "button": "approve"}),
        intent,
        expect("circle", &[("claude", "done")], &[]),
    ));

    let played = transcript("J5", "Soft limit", &steps);
    core.app.as_ref().expect("the app").at_home(&played)
}

/// A made-up Anthropic key, so Claude is in the corral.
const J5_KEY: &str = "sk-ant-journeyNotARealKey3Hk8Nv1Qz6Tb"; // gitleaks:allow (made-up test key)

#[tokio::test(start_paused = true)]
async fn j5_a_run_at_its_soft_limit_asks_and_goes_on_when_approved() {
    check("J5", play_j5).await;
}

// ---- J4: a run's trace in the main window ----------------------------------------------------

const J4_TASK: &str = "Find my notes on the trip";

/// Reading files under the default policy, which allows it without asking.
fn reading_tools(desk: &Arc<CheckpointDesk>) -> ToolSetup {
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Plain {
            name: "fs.read",
            class: ActionClass::ReadLocal,
            answer: "Lisbon, Dec 12 to 19. Budget: $2,400.",
        }))
        .expect("register");
    let n = Arc::new(AtomicU64::new(0));
    ToolSetup {
        policy: Policy::new(Profile::default()),
        gateway: Arc::new(gateway),
        desk: Arc::clone(desk),
        ids: Arc::new(move || format!("j4-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
    }
}

/// J4 (`docs/04-test-plan.md` §6): while Claude works, click its task card in the trace tree;
/// the main window comes forward with that run open; show only its tool calls; copy its events
/// as JSON.
async fn play_j4() -> String {
    let desk = Arc::new(CheckpointDesk::default());
    let script = FakeScript::default()
        .say("Looking for the trip notes.")
        .tool("fs.read", json!({"path": "trip.md"}))
        .say(" Found the dates. Checking the budget.")
        .tool("fs.read", json!({"path": "budget.md"}))
        .say(" Writing it up.")
        .wait(Duration::from_secs(120))
        .say(" Lisbon, Dec 12 to 19, on $2,400.");
    let app = AppSide::new("j4");
    app.keys
        .set(
            Provider::Anthropic,
            &corral_secrets::Secret::new(J5_KEY).expect("a key"),
        )
        .expect("stored");
    let core = Core::with_app(
        Arc::new(
            FakeDriver::new(FakeScript::default())
                .with_prompt(J4_TASK, script)
                .with_tools(reading_tools(&desk)),
        ),
        desk,
        app,
    );
    let app = core.app.as_ref().expect("the app");
    let none = Value::Null;
    let working = || {
        with_column_status(
            expect("circle", &[("claude", "working")], &["claude"]),
            &[("claude", "working")],
        )
    };
    let mut steps = Vec::new();

    steps.push(checked(
        &core,
        "The corral opens with Claude",
        none.clone(),
        none.clone(),
        expect("circle", &[("claude", "available")], &[]),
    ));

    let intent =
        json!({"v": 1, "intent": {"kind": "deployTask", "id": "claude", "prompt": J4_TASK}});
    core.intent(&intent).await;
    wait(2).await;
    steps.push(checked(
        &core,
        "Give Claude a task: it reads two files and keeps working, its column up",
        json!({"kind": "task", "agent": "claude", "text": J4_TASK}),
        intent,
        working(),
    ));

    let intent = json!({"v": 1, "intent": {"kind": "openTrace", "id": "claude"}});
    core.intent(&intent).await;
    steps.push(checked(
        &core,
        "Click Claude's task card in the trace tree: the main window comes forward",
        json!({"kind": "openTraceCard", "agent": "claude"}),
        intent,
        working(),
    ));

    let asked = app.traces.take_open_run();
    let run = asked.clone().expect("the viewer is asked for a run");
    steps.push(in_window(
        &core,
        "The trace viewer takes the run the card asked for, once",
        json!({"kind": "viewerOpens"}),
        json!({"command": "take_open_run", "args": {}, "answer": asked}),
        working(),
    ));

    let runs = app.traces.recent_runs().expect("the runs");
    assert_eq!(
        runs.first().map(|r| r.run_id.as_str()),
        Some(run.as_str()),
        "newest first"
    );
    steps.push(in_window(
        &core,
        "It lists the newest runs, Claude's first and still running",
        json!({"kind": "viewerOpens"}),
        json!({"command": "recent_runs", "args": {}, "answer": answer(&runs)}),
        working(),
    ));

    let trace = app.traces.run_trace(&run).expect("the trace");
    let tool_spans: Vec<String> = trace
        .view
        .spans
        .iter()
        .filter(|span| answer(span.kind) == json!("tool"))
        .map(|span| span.id.clone())
        .collect();
    assert_eq!(tool_spans.len(), 2, "two tool calls");
    let run_span = trace
        .view
        .spans
        .iter()
        .find(|span| answer(span.kind) == json!("run"))
        .map(|span| span.id.clone())
        .expect("the run's span");
    steps.push(in_window(
        &core,
        "It opens Claude's run: its model call and its two reads",
        json!({"kind": "viewerOpens"}),
        json!({"command": "run_trace", "args": {"runId": run}, "answer": answer(&trace)}),
        working(),
    ));

    let mut shown = vec![run_span];
    shown.extend(tool_spans);
    steps.push(in_window(
        &core,
        "Show Tool calls: the run and its two reads",
        json!({"kind": "show", "filter": "Tool calls", "spans": shown}),
        json!({"command": null}),
        working(),
    ));

    steps.push(in_window(
        &core,
        "Copy as JSON: the run's events go to the clipboard",
        json!({"kind": "copyJson"}),
        json!({"command": "clipboard", "args": {"text": trace.json}}),
        working(),
    ));

    let played = transcript("J4", "Trace viewer", &steps);
    app.at_home(&played)
}

#[tokio::test(start_paused = true)]
async fn j4_a_task_cards_click_opens_its_run_filtered_and_copied_in_the_main_window() {
    check("J4", play_j4).await;
}
