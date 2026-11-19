//! Claude's tools (TP-TOOL-16..18): the Messages request with tools, the conversation as
//! Anthropic expects it (`tool_use` blocks from the assistant, `tool_result` blocks from the
//! user), reading a streamed `tool_use` block, and the same agent loop Grok uses, driving Claude.

use corral_adapters::agent::{FunctionCall, MAX_CALL_ARGUMENT_BYTES, MessagesAgent, ToolSetup};
use corral_adapters::cassette::{Cassette, CassetteTransport, RecordedResponse};
use corral_adapters::messages::{MessageDecoder, MessagesConfig, MessagesConversation};
use corral_adapters::sse::SseEvent;
use corral_adapters::stream::StreamDecoder;
use corral_core::budget::BudgetConfig;
use corral_core::clock::ManualClock;
use corral_core::domain::ActionClass;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use corral_tools::{
    Answer, BoxFuture, CheckpointDesk, OfferedTool, Tool, ToolError, ToolGateway, ToolSpec,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1-Wq9z"; // gitleaks:allow (made-up test key)

fn config() -> MessagesConfig {
    MessagesConfig::anthropic("claude-test")
}

fn offered() -> Vec<OfferedTool> {
    vec![
        OfferedTool {
            wire_name: "fs_read".into(),
            name: "fs.read".into(),
            description: "Read a file.".into(),
            parameters: json!({"type": "object",
                "properties": {"path": {"type": "string", "description": "A path."}},
                "required": ["path"], "additionalProperties": false}),
        },
        OfferedTool {
            wire_name: "fs_write".into(),
            name: "fs.write".into(),
            description: "Write a file.".into(),
            parameters: json!({"type": "object"}),
        },
    ]
}

fn call(id: &str, name: &str, arguments: &str) -> FunctionCall {
    FunctionCall {
        call_id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn sorted_keys(body: &Value) -> Vec<String> {
    let mut keys: Vec<String> = body.as_object().expect("object").keys().cloned().collect();
    keys.sort_unstable();
    keys
}

// ---- TP-TOOL-16: the request and the conversation ------------------------------------------------

#[test]
fn tp_tool_16_a_request_offers_claudes_tools_one_call_at_a_time() {
    let body = config().turn_body(
        &MessagesConversation::new("Tidy my notes."),
        &offered(),
        4_000,
    );
    assert_eq!(
        sorted_keys(&body),
        [
            "max_tokens",
            "messages",
            "model",
            "stream",
            "tool_choice",
            "tools"
        ]
    );
    assert_eq!(body["max_tokens"], 4_000);
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["tool_choice"],
        json!({"type": "auto", "disable_parallel_tool_use": true})
    );
    assert_eq!(
        body["tools"],
        json!([
            {"name": "fs_read", "description": "Read a file.",
             "input_schema": offered()[0].parameters},
            {"name": "fs_write", "description": "Write a file.",
             "input_schema": {"type": "object"}},
        ])
    );
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "Tidy my notes."}])
    );
}

#[test]
fn tp_tool_16_with_no_tools_nothing_tool_shaped_is_sent() {
    let body = config().turn_body(&MessagesConversation::new("Hi."), &[], 100);
    assert_eq!(
        sorted_keys(&body),
        ["max_tokens", "messages", "model", "stream"]
    );
}

#[test]
fn tp_tool_16_a_turn_goes_back_as_tool_use_then_tool_result() {
    let mut c = MessagesConversation::new("Tidy my notes.");
    c.turn(
        "Let me look.",
        &[(
            call("toolu_1", "fs_read", r#"{"path":"notes.md"}"#),
            "buy milk".into(),
        )],
    );
    c.turn(
        "",
        &[
            (call("toolu_2", "fs_read", r#"{"path":"a.md"}"#), "A".into()),
            (call("toolu_3", "fs_read", "not json"), "Bad.".into()),
        ],
    );
    assert_eq!(
        c.messages(),
        [
            json!({"role": "user", "content": "Tidy my notes."}),
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "Let me look."},
                {"type": "tool_use", "id": "toolu_1", "name": "fs_read",
                 "input": {"path": "notes.md"}},
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "buy milk"},
            ]}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_2", "name": "fs_read", "input": {"path": "a.md"}},
                {"type": "tool_use", "id": "toolu_3", "name": "fs_read", "input": {}},
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_2", "content": "A"},
                {"type": "tool_result", "tool_use_id": "toolu_3", "content": "Bad."},
            ]}),
        ],
        "no empty text block; input that isn't an object goes back as an empty object"
    );
    let body = config().turn_body(&c, &offered(), 100);
    assert_eq!(body["messages"], json!(c.messages()));
}

#[test]
fn tp_tool_16_the_budget_sees_the_conversation_and_the_tools() {
    let short = MessagesConversation::new("Hi.");
    let mut long = short.clone();
    long.turn(
        "",
        &[(call("toolu_1", "fs_read", "{}"), "x".repeat(10_000))],
    );
    let bare = config().turn_estimate(&short, &[]);
    let with_tools = config().turn_estimate(&short, &offered());
    let after = config().turn_estimate(&long, &offered());
    assert!(with_tools.input_tokens > bare.input_tokens);
    assert!(after.input_tokens >= with_tools.input_tokens + 10_000);
    assert_eq!(after.max_output_tokens, config().max_output_tokens);
}

// ---- TP-TOOL-17: reading Claude's calls ----------------------------------------------------------

fn event(data: Value) -> SseEvent {
    SseEvent {
        event: data["type"].as_str().map(str::to_owned),
        data: data.to_string(),
    }
}

fn feed(d: &mut MessageDecoder, events: &[Value]) -> Vec<EventBody> {
    events
        .iter()
        .flat_map(|e| d.on_event(&event(e.clone())))
        .collect()
}

fn kinds(out: &[EventBody]) -> Vec<&'static str> {
    out.iter().map(|b| b.kind().as_str()).collect()
}

fn start() -> Value {
    json!({"type": "message_start", "message": {"usage": {"input_tokens": 120, "output_tokens": 1}}})
}

fn tool_start(index: u64, id: &str, name: &str) -> Value {
    json!({"type": "content_block_start", "index": index,
           "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}})
}

fn json_piece(index: u64, piece: &str) -> Value {
    json!({"type": "content_block_delta", "index": index,
           "delta": {"type": "input_json_delta", "partial_json": piece}})
}

fn stop(index: u64) -> Value {
    json!({"type": "content_block_stop", "index": index})
}

fn end(reason: &str) -> [Value; 2] {
    [
        json!({"type": "message_delta", "delta": {"stop_reason": reason},
               "usage": {"output_tokens": 40}}),
        json!({"type": "message_stop"}),
    ]
}

#[test]
fn tp_tool_17_a_tool_use_block_is_read_whole_and_the_run_goes_on() {
    let mut d = MessageDecoder::offering_tools(config().price);
    let mut events = vec![
        start(),
        json!({"type": "content_block_start", "index": 0,
               "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": "Let me save that."}}),
        stop(0),
        tool_start(1, "toolu_1", "fs_write"),
        json_piece(1, "{\"path\":"),
        json_piece(1, "\"todo.md\",\"content\":\"buy milk\"}"),
        stop(1),
    ];
    events.extend(end("tool_use"));
    let out = feed(&mut d, &events);
    assert_eq!(
        kinds(&out),
        ["model.delta", "model.response", "usage.reported"],
        "no run.completed: the loop runs the call and asks again"
    );
    assert!(d.is_finished());
    assert_eq!(d.said(), "Let me save that.");
    assert_eq!(
        d.take_calls(),
        [call(
            "toolu_1",
            "fs_write",
            r#"{"path":"todo.md","content":"buy milk"}"#
        )]
    );
    assert!(d.take_calls().is_empty(), "taken once");
}

#[test]
fn tp_tool_17_a_call_with_no_input_has_an_empty_object_and_calls_keep_their_order() {
    let mut d = MessageDecoder::offering_tools(config().price);
    let mut events = vec![
        start(),
        tool_start(0, "toolu_1", "fs_read"),
        stop(0),
        tool_start(1, "toolu_2", "fs_read"),
        json_piece(1, r#"{"path":"b.md"}"#),
        stop(1),
    ];
    events.extend(end("tool_use"));
    feed(&mut d, &events);
    assert_eq!(
        d.take_calls(),
        [
            call("toolu_1", "fs_read", "{}"),
            call("toolu_2", "fs_read", r#"{"path":"b.md"}"#),
        ]
    );
}

#[test]
fn tp_tool_17_text_with_no_call_completes_the_run_as_before() {
    let mut d = MessageDecoder::offering_tools(config().price);
    let mut events = vec![
        start(),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": "All tidy."}}),
    ];
    events.extend(end("end_turn"));
    let out = feed(&mut d, &events);
    assert_eq!(
        kinds(&out),
        [
            "model.delta",
            "model.response",
            "usage.reported",
            "run.completed"
        ]
    );
    assert!(d.take_calls().is_empty());
}

fn single_failure(out: &[EventBody]) -> (FailureReason, String) {
    match out {
        [EventBody::RunFailed(f)] => (f.reason, f.message.clone()),
        other => panic!("expected one run.failed, got {:?}", kinds(other)),
    }
}

#[test]
fn tp_tool_17_a_tool_that_runs_at_anthropic_is_still_refused() {
    let mut d = MessageDecoder::offering_tools(config().price);
    let out = feed(
        &mut d,
        &[
            start(),
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}}),
        ],
    );
    let (reason, message) = single_failure(&out);
    assert_eq!(reason, FailureReason::ToolsNotEnabled);
    assert!(message.contains("web_search"), "{message}");
    assert!(d.take_calls().is_empty());
}

#[test]
fn tp_tool_17_a_broken_call_fails_the_run_and_nothing_is_kept() {
    let huge = format!(
        "{{\"content\":\"{}\"}}",
        "x".repeat(MAX_CALL_ARGUMENT_BYTES)
    );
    let cases: Vec<Vec<Value>> = vec![
        vec![tool_start(0, "", "fs_read"), stop(0)],
        vec![tool_start(0, "toolu_1", ""), stop(0)],
        vec![
            tool_start(0, "toolu_1", "fs_write"),
            json_piece(0, &huge),
            stop(0),
        ],
    ];
    for case in cases {
        let mut d = MessageDecoder::offering_tools(config().price);
        let mut events = vec![start()];
        events.extend(case);
        events.extend(end("tool_use"));
        let out = feed(&mut d, &events);
        let (reason, _) = single_failure(&out);
        assert_eq!(reason, FailureReason::Adapter);
        assert!(d.is_finished());
        assert!(d.take_calls().is_empty());
    }
}

#[test]
fn tp_tool_17_input_right_at_the_limit_is_kept() {
    let padding = MAX_CALL_ARGUMENT_BYTES - r#"{"content":""}"#.len();
    let at_limit = format!(r#"{{"content":"{}"}}"#, "x".repeat(padding));
    let mut d = MessageDecoder::offering_tools(config().price);
    let mut events = vec![
        start(),
        tool_start(0, "toolu_1", "fs_write"),
        json_piece(0, &at_limit),
        stop(0),
    ];
    events.extend(end("tool_use"));
    let out = feed(&mut d, &events);
    assert_eq!(kinds(&out), ["model.response", "usage.reported"]);
    assert_eq!(d.take_calls().len(), 1);
}

// ---- TP-TOOL-18: the agent loop, driving Claude --------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

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

fn sse(events: &[Value]) -> RecordedResponse {
    RecordedResponse {
        status: 200,
        retry_after_secs: None,
        stall_after_body: false,
        body: events
            .iter()
            .flat_map(|e| {
                [
                    format!("event: {}", e["type"].as_str().expect("type")),
                    format!("data: {e}"),
                    String::new(),
                ]
            })
            .collect(),
    }
}

fn calls_tool(id: &str, name: &str, input: &str) -> RecordedResponse {
    let mut events = vec![
        start(),
        tool_start(0, id, name),
        json_piece(0, input),
        stop(0),
    ];
    events.extend(end("tool_use"));
    sse(&events)
}

fn says(text: &str) -> RecordedResponse {
    let mut events = vec![
        start(),
        json!({"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": text}}),
    ];
    events.extend(end("end_turn"));
    sse(&events)
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    transport: Arc<CassetteTransport>,
    reads: Arc<Mutex<Vec<Value>>>,
    writes: Arc<Mutex<Vec<Value>>>,
}

fn world(responses: Vec<RecordedResponse>) -> World {
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
            answer: "Wrote 8 bytes to todo.md.",
            calls: Arc::clone(&writes),
        }))
        .expect("register");
    let desk = Arc::new(CheckpointDesk::default());
    let keys = MemoryStore::default();
    keys.set(Provider::Anthropic, &Secret::new(KEY).expect("key"))
        .expect("store");
    let transport = Arc::new(CassetteTransport::new(
        Cassette {
            description: "Claude's agent loop".into(),
            responses,
        },
        4096,
    ));
    let n = Arc::new(AtomicU64::new(0));
    let agent = MessagesAgent::new(
        transport.clone(),
        Arc::new(keys),
        config(),
        ToolSetup {
            policy: Policy::new(Profile::from_toml("").expect("profile")),
            gateway: Arc::new(gateway),
            desk: Arc::clone(&desk),
            ids: Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1)),
        },
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(agent),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("claude"),
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

impl World {
    fn kinds(&self) -> Vec<&'static str> {
        self.log
            .0
            .lock()
            .expect("log")
            .iter()
            .map(|e| e.kind().as_str())
            .collect()
    }

    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(600), self.sup.wait_idle())
            .await
            .expect("the run ends");
        let mut state = Lifecycle::idle();
        for e in self.log.0.lock().expect("log").iter() {
            state = next(&state, e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    /// The tool_result blocks the n-th request sent last.
    fn results(&self, request: usize) -> Value {
        let seen = self.transport.seen();
        let messages = seen[request].body["messages"]
            .as_array()
            .expect("messages")
            .clone();
        messages.last().expect("a message")["content"].clone()
    }
}

#[tokio::test(start_paused = true)]
async fn tp_tool_18_claude_reads_a_file_and_hears_what_it_says() {
    let w = world(vec![
        calls_tool("toolu_1", "fs_read", r#"{"path":"notes.md"}"#),
        says("Your notes say to buy milk."),
    ]);
    w.idle().await;
    assert_eq!(
        *w.reads.lock().expect("reads"),
        [json!({"path": "notes.md"})]
    );
    assert_eq!(w.kinds().last(), Some(&"run.completed"), "{:?}", w.kinds());
    let seen = w.transport.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        let names: Vec<&str> = request.body["tools"]
            .as_array()
            .expect("tools offered")
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, ["fs_read", "fs_write"]);
    }
    assert_eq!(
        w.results(1),
        json!([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "buy milk"}])
    );
    assert_eq!(
        seen[0].body["messages"],
        json!([{"role": "user", "content": "Tidy my notes."}]),
        "the first turn carries the prompt"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_tool_18_claudes_write_waits_on_the_card_until_approved() {
    let w = world(vec![
        calls_tool(
            "toolu_1",
            "fs_write",
            r#"{"path":"todo.md","content":"buy milk"}"#,
        ),
        says("Saved."),
    ]);
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    let waiting = w.desk.waiting();
    let [checkpoint] = waiting.as_slice() else {
        panic!("one checkpoint: {:?}", w.kinds());
    };
    assert_eq!(
        w.transport.seen().len(),
        1,
        "nothing more is sent while it waits"
    );
    assert!(w.writes.lock().expect("writes").is_empty());
    w.desk
        .answer(
            checkpoint,
            Answer::Approve {
                how: Approval::Click,
                scope: Scope::Once,
            },
        )
        .expect("answered");
    w.idle().await;
    assert_eq!(w.writes.lock().expect("writes").len(), 1);
    assert_eq!(
        w.results(1),
        json!([{"type": "tool_result", "tool_use_id": "toolu_1",
                "content": "Wrote 8 bytes to todo.md."}])
    );
    assert_eq!(w.kinds().last(), Some(&"run.completed"));
}

#[test]
fn tp_tool_17_oversized_input_says_how_big_it_was() {
    let huge = format!(
        "{{\"content\":\"{}\"}}",
        "x".repeat(MAX_CALL_ARGUMENT_BYTES)
    );
    let mut d = MessageDecoder::offering_tools(config().price);
    let out = feed(
        &mut d,
        &[
            start(),
            tool_start(0, "toolu_1", "fs_write"),
            json_piece(0, &huge),
        ],
    );
    let (_, message) = single_failure(&out);
    assert!(message.contains("1024 KB of arguments"), "{message}");
    assert!(message.contains("the most is 1024 KB"), "{message}");
}
