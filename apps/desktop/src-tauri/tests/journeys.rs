//! End-to-end journeys as transcripts (Spike S2, `docs/04-test-plan.md` §6). A journey is played
//! through the real core: the supervisor, the corral's controls and what the corral shows
//! ([`CorralState`], the same state the app's feed holds), with `FakeAdapter` scripts in place of
//! providers and a clock that moves only with tokio's paused time. Each user gesture is the
//! intent JSON Swift sends, decoded by the bridge; each change the core makes is recorded as the
//! roster Swift gets.
//!
//! The recording is the journey's transcript, checked in beside the bridge fixtures
//! (`native/CorralNative/Fixtures/journeys/`). This test fails when the core no longer produces
//! it, so a change shows up as a reviewed diff; `CorralJourneyTests.swift` replays it into
//! CorralNative, performs each gesture on the corral's own models and checks the intent and what
//! the corral shows. Regenerate with
//! `CORRAL_WRITE_FIXTURES=1 cargo test -p corral-desktop --test journeys`.
//!
//! No keys and no network (AGENTS.md rule 7), and no gate bypass (rule 4): the agents here call
//! no tools.

use corral_adapters::catalog::agent_for;
use corral_adapters::fake::{FakeDriver, FakeScript};
use corral_adapters::linked::LinkedSettings;
use corral_core::budget::BudgetConfig;
use corral_core::clock::Clock;
use corral_core::controls::CorralControls;
use corral_core::event::Event;
use corral_core::supervisor::{EventLog, LogError, Supervisor, SupervisorConfig, WatchedLog};
use corral_desktop_lib::bridge::{Intent, decode_intent};
use corral_desktop_lib::corral_order::CorralOrder;
use corral_desktop_lib::corral_state::CorralState;
use corral_secrets::Provider;
use serde_json::{Value, json};
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

/// The core as the app wires it, recording every roster it sends.
struct Core {
    state: Arc<Mutex<CorralState>>,
    controls: CorralControls,
    sent: Arc<Mutex<Vec<Value>>>,
    clock: Arc<PausedClock>,
}

fn roster_json(state: &CorralState, now_ms: u64) -> Value {
    let bytes = state.snapshot(now_ms).to_bytes().expect("roster");
    serde_json::from_slice(&bytes).expect("roster JSON")
}

impl Core {
    fn new(driver: FakeDriver, agents: &[Provider]) -> Self {
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
            Arc::new(driver),
            log,
            Arc::clone(&clock) as Arc<dyn Clock>,
        );
        let ids = Arc::new(AtomicU64::new(1));
        let controls = CorralControls::new(
            supervisor,
            agents.iter().map(|p| agent_for(*p).id),
            Arc::new(move || format!("j-{}", ids.fetch_add(1, Ordering::SeqCst))),
        );
        let core = Self {
            state,
            controls,
            sent,
            clock,
        };
        // The roster the corral opens with (the app's `publish_now`).
        let opening = roster_json(&core.state.lock().expect("state"), core.clock.now_ms());
        core.sent.lock().expect("sent").push(opening);
        core
    }

    /// A gesture's intent, as Swift sends it, through the bridge to the controls.
    async fn intent(&self, wire: &Value) {
        let bytes = serde_json::to_vec(wire).expect("intent");
        let intent = decode_intent(&bytes).expect("the bridge takes it");
        // As the app's `on_intent` does, for the intents the journeys use so far.
        if let Intent::DeployTask { agent, prompt } = &intent {
            self.controls.deploy(agent, prompt).expect("deployed");
        } else if let Intent::SendBack(agent) = &intent {
            self.controls.send_back(agent).expect("sent back");
        } else {
            panic!("not in a journey yet: {intent:?}");
        }
        settle().await;
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
        std::mem::take(&mut *self.sent.lock().expect("sent"))
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
    expect: Value,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({
            "say": self.say,
            "gesture": self.gesture,
            "intent": self.intent,
            "rosters": self.rosters,
            "expect": self.expect,
        })
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

fn checked(
    say: &'static str,
    gesture: Value,
    intent: Value,
    rosters: Vec<Value>,
    expected: Value,
) -> Step {
    let last = rosters
        .last()
        .unwrap_or_else(|| panic!("{say}: the core sent no roster"));
    let drops: Vec<Value> = last["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|a| json!({"id": a["id"], "status": a["status"]}))
        .collect();
    assert_eq!(Value::Array(drops), expected["drops"], "{say}: the drops");
    let columns: Vec<Value> = last["trace"]["columns"]
        .as_array()
        .map(|cs| cs.iter().map(|c| c["id"].clone()).collect())
        .unwrap_or_default();
    assert_eq!(
        Value::Array(columns),
        expected["columns"],
        "{say}: the trace columns"
    );
    Step {
        say,
        gesture,
        intent,
        rosters,
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
        FakeDriver::new(FakeScript::default()).with_prompt(J9_TASK, script),
        &[Provider::Xai, Provider::OpenAi, Provider::Anthropic],
    );
    let none = Value::Null;
    let mut steps = Vec::new();

    steps.push(checked(
        "The corral opens with Grok, GPT and Claude",
        none.clone(),
        none.clone(),
        core.take_sent(),
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
        "Give Grok a task in its card: still a triangle, its ring turns, its column rises",
        json!({"kind": "task", "agent": "grok", "text": J9_TASK}),
        intent,
        core.take_sent(),
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
        "Connect a Meta key: Muse Spark joins and the corral becomes a square",
        none.clone(),
        none.clone(),
        core.take_sent(),
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
        "Sign out of ChatGPT: GPT leaves and the corral is a triangle again",
        none.clone(),
        none.clone(),
        core.take_sent(),
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
        "Grok finishes: its drop shows it's done, and its column folds away",
        none.clone(),
        none.clone(),
        core.take_sent(),
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
        "Send Grok back from its card: still a triangle, and Grok is ready for a task",
        json!({"kind": "press", "agent": "grok", "button": "sendBack"}),
        intent,
        core.take_sent(),
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
    let played = play_j9().await;
    assert_eq!(play_j9().await, played, "the same on every run");
    matches_checked_in("J9", &played);
}
