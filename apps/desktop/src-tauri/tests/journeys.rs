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
use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_adapters::linked::LinkedSettings;
use corral_core::budget::BudgetConfig;
use corral_core::clock::Clock;
use corral_core::controls::CorralControls;
use corral_core::domain::ActionClass;
use corral_core::event::Event;
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::supervisor::{
    EventLog, LogError, RunDriver, Supervisor, SupervisorConfig, WatchedLog,
};
use corral_desktop_lib::bridge::{Intent, decode_intent};
use corral_desktop_lib::corral_order::CorralOrder;
use corral_desktop_lib::corral_state::CorralState;
use corral_desktop_lib::prompt_bar_feed;
use corral_policy::{Policy, Profile};
use corral_secrets::Provider;
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
}

fn roster_json(state: &CorralState, now_ms: u64) -> Value {
    let bytes = state.snapshot(now_ms).to_bytes().expect("roster");
    serde_json::from_slice(&bytes).expect("roster JSON")
}

impl Core {
    fn new(driver: Arc<dyn RunDriver>, desk: Arc<CheckpointDesk>, agents: &[Provider]) -> Self {
        let clock = Arc::new(PausedClock(tokio::time::Instant::now()));
        let nonce = Arc::new(AtomicU64::new(1));
        let state = CorralState::open(
            CorralOrder::open(None),
            cards(agents),
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
            Arc::new(WatchedLog::new(
                Arc::new(MemLog::default()),
                move |event: &Event| {
                    let mut state = state.lock().expect("state");
                    let now = clock.now_ms();
                    let (changed, _notices) = state.observe(event, now);
                    if changed {
                        sent.lock().expect("sent").push(roster_json(&state, now));
                    }
                },
            ))
        };
        let supervisor = Supervisor::with_budget(
            SupervisorConfig::default(),
            BudgetConfig::default(),
            driver,
            log,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let ids = Arc::new(AtomicU64::new(1));
        let controls = CorralControls::new(
            supervisor,
            agents.iter().map(|p| agent_for(*p).id),
            Arc::new(move || format!("j-{}", ids.fetch_add(1, Ordering::SeqCst))),
        );
        let opening = roster_json(&state.lock().expect("state"), clock.now_ms());
        let core = Self {
            state,
            controls,
            desk,
            sent,
            bars: Mutex::new(Vec::new()),
            last: Mutex::new(opening.clone()),
            clock,
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
            | Intent::StopAll
            | Intent::CorralIdle(_)
            | Intent::ApproveWithTouchId { .. }
            | Intent::OpenTrace(_)) => panic!("not in a journey yet: {other:?}"),
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
        self.controls
            .set_agents(agents.iter().map(|p| agent_for(*p).id));
        let mut state = self.state.lock().expect("state");
        let now = self.clock.now_ms();
        if state.set_agents(cards(agents), LinkedSettings::default(), now) {
            self.sent
                .lock()
                .expect("sent")
                .push(roster_json(&state, now));
        }
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
        // Only a step the prompt bar answered has bars.
        if !self.bars.is_empty() {
            step["bars"] = json!(self.bars);
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
    Step {
        say,
        gesture,
        intent,
        rosters,
        bars,
        expect: expected,
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
