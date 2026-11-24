//! Grok with tools: the agent loop (M3 item 3, TP-TOOL-07..12). Each turn is a Responses request
//! admitted by the budget; each function call the model makes goes through the agent's policy,
//! the checkpoint desk and the tool gateway, exactly like the fake agent's (docs/01 §5); what came
//! back goes into the next turn. Cassettes stand in for xAI (AGENTS.md rule 7), and time is tokio's
//! paused clock.

use corral_adapters::agent::{DEFAULT_MAX_TURNS, ResponsesAgent};
use corral_adapters::cassette::{Cassette, CassetteTransport, RecordedResponse};
use corral_adapters::fake::ToolSetup;
use corral_adapters::responses::ResponsesConfig;
use corral_core::budget::BudgetConfig;
use corral_core::clock::ManualClock;
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway, ToolSpec};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

// ---- Harness ----------------------------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log").clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(|e| e.kind().as_str()).collect()
    }

    fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| **k == kind).count()
    }

    fn terminal(&self) -> EventBody {
        self.events().last().cloned().expect("events").body
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    fn opened(&self) -> Vec<CheckpointOpened> {
        self.events()
            .into_iter()
            .filter_map(|e| {
                let EventBody::CheckpointOpened(c) = e.body else {
                    return None;
                };
                Some(c)
            })
            .collect()
    }
}

/// A tool with a spec that records its calls and answers with a fixed text.
struct Recorder {
    name: &'static str,
    class: ActionClass,
    answer: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl Tool for Recorder {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["path"].as_str().map(str::to_owned)
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec {
            description: format!("The {} test tool.", self.name),
            parameters: json!({"type": "object"}),
        })
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        self.calls.lock().expect("calls").push(args.clone());
        Box::pin(async move { Ok(self.answer.to_owned()) })
    }
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    transport: Arc<CassetteTransport>,
    reads: Arc<Mutex<Vec<Value>>>,
    writes: Arc<Mutex<Vec<Value>>>,
}

fn ids() -> Arc<dyn Fn() -> String + Send + Sync> {
    let n = Arc::new(AtomicU64::new(0));
    Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

struct Setup {
    max_turns: Option<u32>,
    soft_limit: Option<u64>,
}

const PLAIN: Setup = Setup {
    max_turns: None,
    soft_limit: None,
};

fn world(responses: Vec<RecordedResponse>, setup: Setup) -> World {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Recorder {
            name: "fs.read",
            class: ActionClass::ReadLocal,
            answer: "buy milk",
            calls: Arc::clone(&reads),
        }))
        .expect("register");
    gateway
        .register(Arc::new(Recorder {
            name: "fs.write",
            class: ActionClass::WriteLocal,
            answer: "Wrote 9 bytes to todo.md.",
            calls: Arc::clone(&writes),
        }))
        .expect("register");
    let desk = Arc::new(CheckpointDesk::default());
    let keys = MemoryStore::default();
    keys.set(Provider::Xai, &Secret::new(KEY).expect("key"))
        .expect("store");
    let transport = Arc::new(CassetteTransport::new(
        Cassette {
            description: "an agent loop".into(),
            responses,
        },
        4096,
    ));
    let mut agent = ResponsesAgent::new(
        transport.clone(),
        Arc::new(keys),
        ResponsesConfig::xai("grok-test"),
        ToolSetup {
            policy: Policy::new(Profile::from_toml("").expect("profile")),
            gateway: Arc::new(gateway),
            desk: Arc::clone(&desk),
            ids: ids(),
        },
    );
    if let Some(n) = setup.max_turns {
        agent = agent.with_max_turns(n);
    }
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(agent),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.set_soft_limit(setup.soft_limit);
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("grok"),
        session_id: SessionId::from("s1"),
        prompt: "Tidy my notes.".into(),
    })
    .expect("deploy");
    World {
        sup,
        log,
        desk,
        transport,
        reads,
        writes,
    }
}

async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

async fn idle(w: &World) {
    tokio::time::timeout(Duration::from_secs(7 * 24 * 3600), w.sup.wait_idle())
        .await
        .expect("the run ends");
    w.log.assert_legal();
}

async fn waiting(w: &World) -> CheckpointOpened {
    settle().await;
    w.log
        .opened()
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no checkpoint: {:?}", w.log.kinds()))
}

fn approve(w: &World, checkpoint: &CheckpointOpened) {
    w.desk
        .answer(
            &checkpoint.checkpoint_id,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("answered");
}

// ---- Cassette building ------------------------------------------------------------------------

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

/// $0.0001 per turn unless a test says otherwise.
const TURN_TICKS: u64 = 1_000_000;

fn completed(ticks: u64) -> Value {
    json!({"type": "response.completed", "response": {"status": "completed",
        "usage": {"input_tokens": 100, "output_tokens": 20, "cost_in_usd_ticks": ticks}}})
}

/// A turn in which the model calls one tool.
fn calls(call_id: &str, name: &str, arguments: &str, ticks: u64) -> RecordedResponse {
    ok(&[
        json!({"type": "response.output_item.added", "item": {"id": "fc", "type": "function_call",
            "call_id": call_id, "name": name, "arguments": ""}}),
        json!({"type": "response.output_item.done", "item": {"id": "fc", "type": "function_call",
            "call_id": call_id, "name": name, "arguments": arguments, "status": "completed"}}),
        completed(ticks),
    ])
}

/// A turn in which the model answers in text.
fn says(text: &str) -> RecordedResponse {
    ok(&[
        json!({"type": "response.output_text.delta", "delta": text}),
        completed(TURN_TICKS),
    ])
}

/// What the n-th request sent as the output of `call_id`.
fn sent_output(w: &World, request: usize, call_id: &str) -> String {
    let seen = w.transport.seen();
    let input = seen[request].body["input"]
        .as_array()
        .expect("input items")
        .clone();
    input
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("no output for {call_id} in request {request}: {input:?}"))
        .to_owned()
}

// ---- TP-TOOL-07: an allowed call runs and its result goes back --------------------------------

#[tokio::test(start_paused = true)]
async fn tp_tool_07_an_allowed_call_runs_and_the_model_hears_the_result() {
    let w = world(
        vec![
            calls("call_1", "fs_read", r#"{"path":"notes.md"}"#, TURN_TICKS),
            says("Your notes say to buy milk."),
        ],
        PLAIN,
    );
    idle(&w).await;
    assert_eq!(
        w.log.kinds(),
        [
            "run.queued",
            "run.started",
            "model.request",
            "model.response",
            "usage.reported",
            "tool.requested",
            "policy.decided",
            "tool.started",
            "tool.result",
            "model.request",
            "model.delta",
            "model.response",
            "usage.reported",
            "run.completed",
        ]
    );
    assert_eq!(
        *w.reads.lock().expect("reads"),
        [json!({"path": "notes.md"})]
    );
    let seen = w.transport.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert_eq!(request.body["store"], false);
        let names: Vec<&str> = request.body["tools"]
            .as_array()
            .expect("tools offered")
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["fs_read", "fs_write"]);
    }
    assert_eq!(
        seen[1].body["input"],
        json!([
            {"role": "user", "content": "Tidy my notes."},
            {"type": "function_call", "call_id": "call_1", "name": "fs_read",
             "arguments": "{\"path\":\"notes.md\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "buy milk"},
        ])
    );
    let EventBody::ToolRequested(requested) = &w.log.events()[5].body else {
        panic!("tool.requested");
    };
    assert_eq!(
        requested.tool, "fs.read",
        "the registered name, not the wire name"
    );
    assert_eq!(requested.action_class, ActionClass::ReadLocal);
}

// ---- TP-TOOL-08: an asked call waits on the card ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_tool_08_an_asked_call_waits_and_runs_once_approved() {
    let w = world(
        vec![
            calls(
                "call_1",
                "fs_write",
                r#"{"path":"todo.md","content":"buy milk"}"#,
                TURN_TICKS,
            ),
            says("Saved."),
        ],
        PLAIN,
    );
    let checkpoint = waiting(&w).await;
    assert_eq!(checkpoint.action_class, ActionClass::WriteLocal);
    assert_eq!(checkpoint.risk, Risk::Normal);
    assert!(
        checkpoint.detail.contains("todo.md"),
        "{}",
        checkpoint.detail
    );
    assert_eq!(
        w.transport.seen().len(),
        1,
        "nothing more is sent while it waits"
    );
    assert!(w.writes.lock().expect("writes").is_empty());
    approve(&w, &checkpoint);
    idle(&w).await;
    assert_eq!(w.writes.lock().expect("writes").len(), 1);
    assert_eq!(sent_output(&w, 1, "call_1"), "Wrote 9 bytes to todo.md.");
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
}

#[tokio::test(start_paused = true)]
async fn tp_tool_08_a_denied_call_doesnt_run_and_the_model_hears_why() {
    let w = world(
        vec![
            calls(
                "call_1",
                "fs_write",
                r#"{"path":"todo.md","content":"buy milk"}"#,
                TURN_TICKS,
            ),
            says("Understood, I won't."),
        ],
        PLAIN,
    );
    let checkpoint = waiting(&w).await;
    w.desk
        .answer(
            &checkpoint.checkpoint_id,
            Answer::Deny {
                note: Some("Keep it in notes.md instead.".into()),
            },
        )
        .expect("answered");
    idle(&w).await;
    assert!(w.writes.lock().expect("writes").is_empty(), "nothing ran");
    let told = sent_output(&w, 1, "call_1");
    assert!(told.contains("You denied this"), "{told}");
    assert!(told.contains("Keep it in notes.md instead."), "{told}");
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
}

// ---- TP-TOOL-09: Stop ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_tool_09_stop_at_a_checkpoint_ends_the_run_and_sends_nothing_more() {
    let w = world(
        vec![
            calls(
                "call_1",
                "fs_write",
                r#"{"path":"todo.md","content":"buy milk"}"#,
                TURN_TICKS,
            ),
            says("never sent"),
        ],
        PLAIN,
    );
    let checkpoint = waiting(&w).await;
    w.sup.cancel(&RunId::from("r1")).await.expect("stop");
    idle(&w).await;
    assert!(matches!(w.log.terminal(), EventBody::RunCancelled(_)));
    assert_eq!(w.transport.seen().len(), 1);
    assert!(w.writes.lock().expect("writes").is_empty());
    assert!(
        w.desk.waiting().is_empty(),
        "Stop takes {:?} off the desk",
        checkpoint.checkpoint_id
    );
}

// ---- TP-TOOL-10: calls Corral can't run ---------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_tool_10_a_tool_the_model_wasnt_given_is_denied_without_asking() {
    let w = world(
        vec![
            calls(
                "call_1",
                "email_send",
                r#"{"to":"someone@example.test"}"#,
                TURN_TICKS,
            ),
            says("I can't send email."),
        ],
        PLAIN,
    );
    idle(&w).await;
    assert!(
        w.log.opened().is_empty(),
        "no checkpoint for an unknown tool"
    );
    let decided: Vec<PolicyDecision> = w
        .log
        .events()
        .into_iter()
        .filter_map(|e| {
            let EventBody::PolicyDecided(p) = e.body else {
                return None;
            };
            Some(p.decision)
        })
        .collect();
    assert!(
        matches!(decided.as_slice(), [PolicyDecision::Deny { .. }]),
        "{decided:?}"
    );
    assert_eq!(w.log.count("tool.started"), 0);
    let told = sent_output(&w, 1, "call_1");
    assert!(!told.is_empty(), "the model is told it didn't run");
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
}

#[tokio::test(start_paused = true)]
async fn tp_tool_10_arguments_that_arent_an_object_run_nothing() {
    for arguments in ["not json", "[1,2]", "\"notes.md\""] {
        let w = world(
            vec![
                calls("call_1", "fs_read", arguments, TURN_TICKS),
                says("Sorry."),
            ],
            PLAIN,
        );
        idle(&w).await;
        assert!(w.reads.lock().expect("reads").is_empty(), "{arguments}");
        assert_eq!(w.log.count("tool.requested"), 0, "{arguments}");
        let told = sent_output(&w, 1, "call_1");
        assert!(told.contains("JSON object"), "{arguments}: {told}");
        assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
    }
}

// ---- TP-TOOL-11: a cap on turns -----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_tool_11_a_run_that_never_finishes_stops_at_its_turn_limit() {
    let w = world(
        (1..=4)
            .map(|n| {
                calls(
                    &format!("call_{n}"),
                    "fs_read",
                    &format!(r#"{{"path":"page{n}.md"}}"#),
                    TURN_TICKS,
                )
            })
            .collect(),
        Setup {
            max_turns: Some(3),
            soft_limit: None,
        },
    );
    idle(&w).await;
    assert_eq!(w.transport.seen().len(), 3, "no fourth request");
    assert_eq!(w.reads.lock().expect("reads").len(), 3);
    let EventBody::RunFailed(f) = w.log.terminal() else {
        panic!("expected run.failed: {:?}", w.log.kinds());
    };
    assert!(f.message.contains("3 turns"), "{}", f.message);
}

#[test]
fn tp_tool_11_the_default_turn_limit() {
    assert_eq!(DEFAULT_MAX_TURNS, 20);
}

// ---- TP-TOOL-12: the soft limit asks in the middle of a task --------------------------------------

/// A turn that bills $0.02, past a $0.01 soft limit.
const PRICEY_TICKS: u64 = 200_000_000;
const SOFT: u64 = 10_000;

fn past_soft() -> World {
    world(
        vec![
            calls("call_1", "fs_read", r#"{"path":"notes.md"}"#, PRICEY_TICKS),
            says("Done."),
        ],
        Setup {
            max_turns: None,
            soft_limit: Some(SOFT),
        },
    )
}

#[tokio::test(start_paused = true)]
async fn tp_tool_12_past_the_soft_limit_the_next_turn_waits_for_approval() {
    let w = past_soft();
    let checkpoint = waiting(&w).await;
    assert_eq!(checkpoint.action_class, ActionClass::BudgetThreshold);
    assert_eq!(w.transport.seen().len(), 1, "the next turn waits");
    approve(&w, &checkpoint);
    idle(&w).await;
    assert_eq!(w.transport.seen().len(), 2);
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
    assert_eq!(sent_output(&w, 1, "call_1"), "buy milk");
}

#[tokio::test(start_paused = true)]
async fn tp_tool_12_denying_the_soft_limit_ends_the_run_and_sends_nothing_more() {
    let w = past_soft();
    let checkpoint = waiting(&w).await;
    w.desk
        .answer(&checkpoint.checkpoint_id, Answer::Deny { note: None })
        .expect("answered");
    idle(&w).await;
    assert_eq!(w.transport.seen().len(), 1);
    assert!(
        matches!(w.log.terminal(), EventBody::RunCancelled(_)),
        "{:?}",
        w.log.kinds()
    );
}

// ---- TP-SUP-06: the agent loop pauses between turns ------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_sup_06_a_pause_asked_for_at_a_checkpoint_takes_hold_before_the_next_turn() {
    let w = world(
        vec![
            calls("call_1", "fs_write", r#"{"path":"todo.md"}"#, TURN_TICKS),
            says("Saved."),
        ],
        PLAIN,
    );
    let checkpoint = waiting(&w).await;
    w.sup.pause(&RunId::from("r1")).expect("pause while asking");
    approve(&w, &checkpoint);
    settle().await;
    assert_eq!(
        w.writes.lock().expect("writes").len(),
        1,
        "the approved call ran"
    );
    assert_eq!(
        w.log.kinds().last(),
        Some(&"run.paused"),
        "{:?}",
        w.log.kinds()
    );
    assert_eq!(w.transport.seen().len(), 1, "the next turn waits");
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(w.transport.seen().len(), 1, "still waiting");
    w.sup.resume(&RunId::from("r1")).expect("resume");
    idle(&w).await;
    assert_eq!(w.transport.seen().len(), 2);
    assert_eq!(w.log.count("run.paused"), 1);
    assert_eq!(w.log.count("run.resumed"), 1);
    assert!(matches!(w.log.terminal(), EventBody::RunCompleted(_)));
}
