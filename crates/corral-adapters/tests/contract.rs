//! The adapter contract suite (docs/04 §2, M3 item 6): the same checks, run for every adapter by
//! `adapter_contract!`. A new adapter joins by implementing [`Subject`] (how to make it answer,
//! try a tool, stall, and go over its cap) and adding one line at the bottom.
//!
//! - **TP-ADP-01** An answer runs `run.queued` → `run.started` → … → exactly one terminal event,
//!   `run.completed`, last; the model is asked (`model.request`) before it says anything; the log
//!   follows the lifecycle.
//! - **TP-ADP-05** The answer's usage is reported after the request.
//! - **TP-ADP-02** Every `tool.started` follows a `policy.decided` for the same call. An adapter
//!   with tools runs an allowed call once and completes; one without fails the run with
//!   `tools_not_enabled` and no tool event.
//! - **TP-ADP-03** Stop ends a run that would never finish within 2 s, with one terminal event.
//! - **TP-BUD-01** Over the run's cap, nothing is sent: no `model.request`, the run fails with
//!   `budget_hard_cap`.
//! - **No key in the log**, for adapters that use one.
//!
//! Real providers run against cassettes (AGENTS.md rule 7); time is tokio's paused clock.

use corral_adapters::agent::ResponsesAgent;
use corral_adapters::cassette::{Cassette, CassetteTransport, RecordedResponse};
use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_core::budget::{BudgetConfig, Price};
use corral_core::clock::ManualClock;
use corral_core::domain::ActionClass;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{
    EventLog, LogError, RunDriver, RunRequest, Supervisor, SupervisorConfig,
};
use corral_policy::{Policy, Profile};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use corral_tools::{BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway, ToolSpec};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const XAI_KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)
const ANTHROPIC_KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1-Wq9z"; // gitleaks:allow (made-up test key)

// ---- The contract ------------------------------------------------------------------------------

/// What kind of run to set up.
enum Scenario {
    /// The model answers in text.
    Answer,
    /// The model asks to read `notes.md`.
    Tool,
    /// The model never finishes until stopped.
    Stall,
    /// The first request is over the run's cap.
    OverCap,
}

/// An adapter under contract.
trait Subject {
    /// Whether the adapter runs tool calls (through the gate) or refuses them.
    const TOOLS: bool;
    /// The run's driver, how many requests reached the provider so far, and the key it used.
    fn driver(
        scenario: &Scenario,
        setup: ToolSetup,
    ) -> (Arc<dyn RunDriver>, Sent, Option<&'static str>);
}

/// How many requests reached the provider (always 0 for an adapter with no provider).
type Sent = Box<dyn Fn() -> usize + Send + Sync>;

/// The runs' cap: $1, or a millionth of a dollar for [`Scenario::OverCap`].
fn cap(scenario: &Scenario) -> u64 {
    match scenario {
        Scenario::OverCap => 1,
        Scenario::Answer | Scenario::Tool | Scenario::Stall => 1_000_000,
    }
}

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

/// A file tool that records its calls.
struct Notes {
    calls: Arc<Mutex<Vec<Value>>>,
}

impl Tool for Notes {
    fn name(&self) -> &str {
        "fs.read"
    }

    fn class(&self) -> ActionClass {
        ActionClass::ReadLocal
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["path"].as_str().map(str::to_owned)
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec {
            description: "Read a file.".into(),
            parameters: json!({"type": "object"}),
        })
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.calls.lock().expect("calls").push(args.clone());
        Box::pin(async { Ok("buy milk".to_owned()) })
    }
}

struct Run {
    sup: Supervisor,
    log: Arc<MemLog>,
    sent: Sent,
    key: Option<&'static str>,
    tool_calls: Arc<Mutex<Vec<Value>>>,
}

impl Run {
    fn events(&self) -> Vec<Event> {
        self.log.0.lock().expect("log").clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(|e| e.kind().as_str()).collect()
    }

    fn terminal(&self) -> EventBody {
        self.events().last().cloned().expect("events").body
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for (i, e) in self.events().iter().enumerate() {
            assert_eq!(e.seq, i as u64 + 1, "seq must be gapless");
            state = next(&state, e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    fn assert_no_key(&self) {
        let Some(key) = self.key else {
            return;
        };
        let all = serde_json::to_string(&self.events()).expect("events serialize");
        for i in 0..=key.len() - 5 {
            assert!(
                !all.contains(&key[i..i + 5]),
                "an event contains part of the key"
            );
        }
    }
}

fn start<S: Subject>(scenario: Scenario) -> Run {
    let tool_calls = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Notes {
            calls: Arc::clone(&tool_calls),
        }))
        .expect("register");
    let n = Arc::new(AtomicU64::new(0));
    let setup = ToolSetup {
        policy: Policy::new(Profile::from_toml("").expect("profile")),
        gateway: Arc::new(gateway),
        desk: Arc::new(CheckpointDesk::default()),
        ids: Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
    };
    let (driver, sent, key) = S::driver(&scenario, setup);
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: cap(&scenario),
        },
        driver,
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("agent"),
        session_id: SessionId::from("s1"),
        prompt: "Read my notes.".into(),
    })
    .expect("deploy");
    Run {
        sup,
        log,
        sent,
        key,
        tool_calls,
    }
}

async fn to_end(run: &Run) {
    tokio::time::timeout(Duration::from_secs(600), run.sup.wait_idle())
        .await
        .expect("the run ends");
    run.assert_legal();
    run.assert_no_key();
}

fn terminal_count(kinds: &[&str]) -> usize {
    kinds
        .iter()
        .filter(|k| matches!(**k, "run.completed" | "run.failed" | "run.cancelled"))
        .count()
}

/// TP-ADP-01 and TP-ADP-05.
async fn answers<S: Subject>() {
    let run = start::<S>(Scenario::Answer);
    to_end(&run).await;
    let kinds = run.kinds();
    assert_eq!(&kinds[..2], ["run.queued", "run.started"], "{kinds:?}");
    assert_eq!(terminal_count(&kinds), 1, "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"run.completed"), "{kinds:?}");
    let asked = kinds
        .iter()
        .position(|k| *k == "model.request")
        .expect("the model is asked");
    for said in ["model.delta", "model.response"] {
        if let Some(at) = kinds.iter().position(|k| *k == said) {
            assert!(asked < at, "{said} before model.request: {kinds:?}");
        }
    }
    let usage = kinds
        .iter()
        .position(|k| *k == "usage.reported")
        .expect("usage is reported");
    assert!(asked < usage, "{kinds:?}");
    assert!(run.tool_calls.lock().expect("calls").is_empty());
}

/// TP-ADP-02.
async fn tools_go_through_the_gate<S: Subject>() {
    let run = start::<S>(Scenario::Tool);
    to_end(&run).await;
    let mut decided: HashSet<ToolCallId> = HashSet::new();
    for e in run.events() {
        if let EventBody::PolicyDecided(p) = &e.body {
            decided.insert(p.tool_call_id.clone());
        }
        if let EventBody::ToolStarted(t) = &e.body {
            assert!(
                decided.contains(&t.tool_call_id),
                "tool.started without policy.decided: {:?}",
                run.kinds()
            );
        }
    }
    let kinds = run.kinds();
    if S::TOOLS {
        assert_eq!(
            *run.tool_calls.lock().expect("calls"),
            [json!({"path": "notes.md"})]
        );
        assert_eq!(kinds.last(), Some(&"run.completed"), "{kinds:?}");
    } else {
        assert!(run.tool_calls.lock().expect("calls").is_empty());
        assert!(!kinds.iter().any(|k| k.starts_with("tool.")), "{kinds:?}");
        let EventBody::RunFailed(f) = run.terminal() else {
            panic!("expected run.failed: {kinds:?}");
        };
        assert_eq!(f.reason, FailureReason::ToolsNotEnabled);
    }
    assert_eq!(terminal_count(&kinds), 1, "{kinds:?}");
}

/// TP-ADP-03.
async fn stops_promptly<S: Subject>() {
    let run = start::<S>(Scenario::Stall);
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert!(run.kinds().contains(&"model.request"), "{:?}", run.kinds());
    assert_eq!(terminal_count(&run.kinds()), 0, "still going");
    let stopped = tokio::time::Instant::now();
    run.sup.cancel(&RunId::from("r1")).await.expect("stop");
    to_end(&run).await;
    assert!(
        stopped.elapsed() <= Duration::from_secs(2),
        "{:?}",
        stopped.elapsed()
    );
    let kinds = run.kinds();
    assert_eq!(kinds.last(), Some(&"run.cancelled"), "{kinds:?}");
    assert_eq!(terminal_count(&kinds), 1, "{kinds:?}");
}

/// TP-BUD-01.
async fn sends_nothing_over_the_cap<S: Subject>() {
    let run = start::<S>(Scenario::OverCap);
    to_end(&run).await;
    let kinds = run.kinds();
    assert!(!kinds.contains(&"model.request"), "{kinds:?}");
    assert_eq!((run.sent)(), 0, "nothing reached the provider");
    let EventBody::RunFailed(f) = run.terminal() else {
        panic!("expected run.failed: {kinds:?}");
    };
    assert_eq!(f.reason, FailureReason::BudgetHardCap);
}

macro_rules! adapter_contract {
    ($name:ident, $subject:ty) => {
        mod $name {
            use super::*;

            #[tokio::test(start_paused = true)]
            async fn tp_adp_01_05_an_answer_runs_in_order_and_reports_usage() {
                answers::<$subject>().await;
            }

            #[tokio::test(start_paused = true)]
            async fn tp_adp_02_tool_calls_go_through_the_gate() {
                tools_go_through_the_gate::<$subject>().await;
            }

            #[tokio::test(start_paused = true)]
            async fn tp_adp_03_stop_ends_the_run_within_2_s() {
                stops_promptly::<$subject>().await;
            }

            #[tokio::test(start_paused = true)]
            async fn tp_bud_01_over_the_cap_nothing_is_sent() {
                sends_nothing_over_the_cap::<$subject>().await;
            }
        }
    };
}

// ---- The adapters ------------------------------------------------------------------------------

fn cassette(provider: &str, name: &str) -> Cassette {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes")
        .join(provider)
        .join(name);
    Cassette::load(&path).expect("cassette loads")
}

fn replay(cassette: Cassette) -> (Arc<CassetteTransport>, Sent) {
    let transport = Arc::new(CassetteTransport::new(cassette, 64));
    let seen = Arc::clone(&transport);
    (transport, Box::new(move || seen.seen().len()))
}

fn keys(provider: Provider, key: &str) -> Arc<MemoryStore> {
    let store = MemoryStore::default();
    store
        .set(provider, &Secret::new(key).expect("key"))
        .expect("store");
    Arc::new(store)
}

/// The fake agent, through the real supervisor, policy, desk and gateway.
struct Fake;

impl Subject for Fake {
    const TOOLS: bool = true;

    fn driver(
        scenario: &Scenario,
        setup: ToolSetup,
    ) -> (Arc<dyn RunDriver>, Sent, Option<&'static str>) {
        let script = match scenario {
            Scenario::Answer | Scenario::OverCap => {
                FakeScript::default().say("Hello there.").usage(10, 5, 7)
            }
            Scenario::Tool => FakeScript::default()
                .tool("fs.read", json!({"path": "notes.md"}))
                .say("Your notes say to buy milk.")
                .usage(10, 5, 7),
            Scenario::Stall => FakeScript::default().wait(Duration::from_secs(3_600)),
        };
        let driver = FakeDriver::new(script)
            .priced(Price::per_million_tokens(1_000_000, 1_000_000))
            .with_tools(setup);
        (Arc::new(driver), Box::new(|| 0), None)
    }
}

fn sse(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .flat_map(|e| {
            [
                format!("event: {}", e["type"].as_str().expect("type")),
                format!("data: {e}"),
                String::new(),
            ]
        })
        .collect()
}

fn ok(events: &[Value]) -> RecordedResponse {
    RecordedResponse {
        status: 200,
        retry_after_secs: None,
        stall_after_body: false,
        body: sse(events),
    }
}

fn completed() -> Value {
    json!({"type": "response.completed", "response": {"status": "completed",
        "usage": {"input_tokens": 100, "output_tokens": 20, "cost_in_usd_ticks": 1_000_000}}})
}

/// Grok with tools: the agent loop over the Responses API.
struct GrokAgent;

impl Subject for GrokAgent {
    const TOOLS: bool = true;

    fn driver(
        scenario: &Scenario,
        setup: ToolSetup,
    ) -> (Arc<dyn RunDriver>, Sent, Option<&'static str>) {
        let tape = match scenario {
            Scenario::Answer | Scenario::OverCap => cassette("xai", "text-ok.json"),
            Scenario::Stall => cassette("xai", "stall.json"),
            Scenario::Tool => Cassette {
                description: "Grok reads notes.md, then answers.".into(),
                responses: vec![
                    ok(&[
                        json!({"type": "response.output_item.done", "item": {
                            "type": "function_call", "call_id": "call_1", "name": "fs_read",
                            "arguments": "{\"path\":\"notes.md\"}", "status": "completed"}}),
                        completed(),
                    ]),
                    ok(&[
                        json!({"type": "response.output_text.delta",
                               "delta": "Your notes say to buy milk."}),
                        completed(),
                    ]),
                ],
            },
        };
        let (transport, sent) = replay(tape);
        let agent = ResponsesAgent::new(
            transport,
            keys(Provider::Xai, XAI_KEY),
            ResponsesConfig::xai("grok-test"),
            setup,
        );
        (Arc::new(agent), sent, Some(XAI_KEY))
    }
}

/// Grok without tools: the walking skeleton's text-only driver.
struct GrokText;

impl Subject for GrokText {
    const TOOLS: bool = false;

    fn driver(
        scenario: &Scenario,
        _setup: ToolSetup,
    ) -> (Arc<dyn RunDriver>, Sent, Option<&'static str>) {
        let tape = match scenario {
            Scenario::Answer | Scenario::OverCap => cassette("xai", "text-ok.json"),
            Scenario::Stall => cassette("xai", "stall.json"),
            Scenario::Tool => cassette("xai", "tool-call.json"),
        };
        let (transport, sent) = replay(tape);
        let driver = ResponsesDriver::new(
            transport,
            keys(Provider::Xai, XAI_KEY),
            ResponsesConfig::xai("grok-test"),
        );
        (Arc::new(driver), sent, Some(XAI_KEY))
    }
}

/// Claude, text only, over the Messages API.
struct ClaudeText;

impl Subject for ClaudeText {
    const TOOLS: bool = false;

    fn driver(
        scenario: &Scenario,
        _setup: ToolSetup,
    ) -> (Arc<dyn RunDriver>, Sent, Option<&'static str>) {
        let tape = match scenario {
            Scenario::Answer | Scenario::OverCap => cassette("anthropic", "text-ok.json"),
            Scenario::Stall => cassette("anthropic", "stall.json"),
            Scenario::Tool => cassette("anthropic", "tool-call.json"),
        };
        let (transport, sent) = replay(tape);
        let driver = MessagesDriver::new(
            transport,
            keys(Provider::Anthropic, ANTHROPIC_KEY),
            MessagesConfig::anthropic("claude-test"),
        );
        (Arc::new(driver), sent, Some(ANTHROPIC_KEY))
    }
}

adapter_contract!(fake, Fake);
adapter_contract!(grok_agent, GrokAgent);
adapter_contract!(grok_text, GrokText);
adapter_contract!(claude_text, ClaudeText);
