//! Claude's web search, the first provider-side tool (M3 item 4, docs/01 §4, TP-PROV-01..09).
//!
//! The search runs on Anthropic's servers, where Corral can't pause it, so it's decided before
//! the run: the agent is offered it only when its policy allows `provider_tools`, and the default
//! denies it. Each search shows in the trace, costs $0.01 within the budget, marks the run as
//! having read untrusted content, and the answer lists its sources. The assistant's blocks go back
//! to Anthropic exactly as they came, encrypted results included, and a paused turn is resumed.
//! No network: the Messages stream is written out as Anthropic documents it.

use corral_adapters::agent::{MessagesAgent, ProviderTool, ToolSetup, allows};
use corral_adapters::cassette::{Cassette, CassetteTransport, RecordedResponse};
use corral_adapters::messages::{
    MessageDecoder, MessagesConfig, MessagesConversation, WEB_SEARCH_MICRO_USD,
    WEB_SEARCHES_PER_TURN,
};
use corral_adapters::sse::SseEvent;
use corral_adapters::stream::StreamDecoder;
use corral_core::budget::BudgetConfig;
use corral_core::clock::ManualClock;
use corral_core::domain::ActionClass;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_policy::{Policy, Profile};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use corral_tools::{
    BoxFuture, CheckpointDesk, OfferedTool, Tool, ToolError, ToolGateway, ToolSpec,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1-Wq9z"; // gitleaks:allow (made-up test key)

/// A profile that lets the agent use provider-side tools.
const ALLOWS_PROVIDER_TOOLS: &str = "[[rules]]\nclass = \"provider_tools\"\ndecision = \"allow\"\n";

/// The search tool as Corral offers it: the basic version, which doesn't run code at Anthropic.
fn search_tool() -> Value {
    json!({"type": "web_search_20250305", "name": "web_search", "max_uses": 5})
}

fn searching() -> MessagesConfig {
    MessagesConfig {
        web_search: true,
        ..MessagesConfig::anthropic("claude-test")
    }
}

fn read_tool() -> OfferedTool {
    OfferedTool {
        wire_name: "fs_read".into(),
        name: "fs.read".into(),
        description: "Read a file.".into(),
        parameters: json!({"type": "object"}),
    }
}

// ---- TP-PROV-01..03: deciding before the run, and the request ----------------------------------

#[test]
fn tp_prov_01_web_search_is_off_unless_the_policy_allows_provider_tools() {
    assert!(
        !MessagesConfig::anthropic("claude-test").web_search,
        "off by default"
    );
    let default = Policy::new(Profile::from_toml("").expect("profile"));
    assert!(!allows(&default, ProviderTool::WebSearch));
    let allowing = Policy::new(Profile::from_toml(ALLOWS_PROVIDER_TOOLS).expect("profile"));
    assert!(allows(&allowing, ProviderTool::WebSearch));
    let asking = Policy::new(
        Profile::from_toml("[[rules]]\nclass = \"provider_tools\"\ndecision = \"ask\"\n")
            .expect("profile"),
    );
    assert!(
        !allows(&asking, ProviderTool::WebSearch),
        "a search can't be paused, so only Allow turns it on"
    );
    assert_eq!(ProviderTool::WebSearch.name(), "web_search");
}

#[test]
fn tp_prov_02_the_request_offers_the_basic_search_after_the_agents_tools() {
    let conversation = MessagesConversation::new("Who was Claude Shannon?");
    let body = searching().turn_body(&conversation, &[read_tool()], 4_000);
    assert_eq!(
        body["tools"],
        json!([
            {"name": "fs_read", "description": "Read a file.", "input_schema": {"type": "object"}},
            search_tool(),
        ])
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "auto", "disable_parallel_tool_use": true})
    );

    let alone = searching().turn_body(&conversation, &[], 4_000);
    assert_eq!(
        alone["tools"],
        json!([search_tool()]),
        "offered with no other tools"
    );
    assert_eq!(alone["tool_choice"]["type"], "auto");

    let off =
        MessagesConfig::anthropic("claude-test").turn_body(&conversation, &[read_tool()], 4_000);
    assert_eq!(
        off["tools"].as_array().map(Vec::len),
        Some(1),
        "only the agent's tool"
    );
    assert!(!off.to_string().contains("web_search"));

    let clash = OfferedTool {
        wire_name: "web_search".into(),
        ..read_tool()
    };
    let both = searching().turn_body(&conversation, &[clash], 4_000);
    assert_eq!(
        both["tools"].as_array().map(Vec::len),
        Some(1),
        "a tool of the agent's already has the name, so the search isn't offered"
    );
}

#[test]
fn tp_prov_03_the_budget_counts_the_most_a_turn_can_search() {
    assert_eq!(WEB_SEARCHES_PER_TURN, 5);
    assert_eq!(WEB_SEARCH_MICRO_USD, 10_000, "$10 per 1,000 searches");
    let conversation = MessagesConversation::new("Who was Claude Shannon?");
    assert_eq!(
        searching()
            .turn_estimate(&conversation, &[])
            .tool_fees_micro_usd,
        50_000
    );
    assert_eq!(
        MessagesConfig::anthropic("claude-test")
            .turn_estimate(&conversation, &[])
            .tool_fees_micro_usd,
        0
    );
}

// ---- TP-PROV-04..07: reading a search --------------------------------------------------------

fn event(data: &Value) -> SseEvent {
    SseEvent {
        event: data["type"].as_str().map(str::to_owned),
        data: data.to_string(),
    }
}

fn feed(d: &mut MessageDecoder, events: &[Value]) -> Vec<EventBody> {
    events.iter().flat_map(|e| d.on_event(&event(e))).collect()
}

fn kinds(out: &[EventBody]) -> Vec<&'static str> {
    out.iter().map(|b| b.kind().as_str()).collect()
}

const URL: &str = "https://en.wikipedia.org/wiki/Claude_Shannon";
const TITLE: &str = "Claude Shannon - Wikipedia";

fn result_item() -> Value {
    json!({"type": "web_search_result", "url": URL, "title": TITLE,
           "encrypted_content": "EqgfCioIARgBIiQ3YTAwMjY1Mi1mZjM5", "page_age": "April 30, 2025"})
}

fn citation() -> Value {
    json!({"type": "web_search_result_location", "url": URL, "title": TITLE,
           "encrypted_index": "Eo8BCioIAhgBIiQyYjQ0OWJmZi1lNm",
           "cited_text": "Claude Elwood Shannon (April 30, 1916 - February 24, 2001)"})
}

/// One response that searches once and answers with a citation, ending with `stop_reason`.
fn search_events(stop_reason: &str) -> Vec<Value> {
    vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 100, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Let me look. "}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {
            "type": "input_json_delta", "partial_json": "{\"query\":"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {
            "type": "input_json_delta", "partial_json": "\"claude shannon born\"}"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "content_block_start", "index": 2, "content_block": {
            "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [result_item()]}}),
        json!({"type": "content_block_stop", "index": 2}),
        json!({"type": "content_block_start", "index": 3, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 3, "delta": {
            "type": "citations_delta", "citation": citation()}}),
        json!({"type": "content_block_delta", "index": 3, "delta": {
            "type": "text_delta", "text": "He was born on April 30, 1916."}}),
        json!({"type": "content_block_stop", "index": 3}),
        json!({"type": "message_delta", "delta": {"stop_reason": stop_reason},
               "usage": {"output_tokens": 40, "server_tool_use": {"web_search_requests": 1}}}),
        json!({"type": "message_stop"}),
    ]
}

#[test]
fn tp_prov_04_a_search_shows_in_the_trace_and_its_cost_is_counted() {
    let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
    let out = feed(&mut d, &search_events("end_turn"));
    assert_eq!(
        kinds(&out),
        [
            "model.delta",
            "tool.requested",
            "policy.decided",
            "tool.started",
            "tool.result",
            "model.delta",
            "model.response",
            "usage.reported",
            "run.completed"
        ]
    );
    let id = ToolCallId::from("srvtoolu_1".to_owned());
    let EventBody::ToolRequested(requested) = &out[1] else {
        panic!("tool.requested");
    };
    assert_eq!(requested.tool_call_id, id);
    assert_eq!(requested.tool, "web_search");
    assert_eq!(requested.action_class, ActionClass::ProviderTools);
    assert_eq!(requested.args, json!({"query": "claude shannon born"}));
    let EventBody::PolicyDecided(decided) = &out[2] else {
        panic!("policy.decided");
    };
    assert_eq!(
        decided.decision,
        PolicyDecision::Allow,
        "allowed before the run"
    );
    let EventBody::ToolResult(result) = &out[4] else {
        panic!("tool.result");
    };
    assert_eq!(result.tool_call_id, id);
    assert!(result.ok);
    assert_eq!(
        result.output,
        json!([{"title": TITLE, "url": URL}]),
        "titles and links only: the page text stays encrypted"
    );
    let EventBody::UsageReported(usage) = &out[7] else {
        panic!("usage.reported");
    };
    // 100 input tokens at $2/M, 40 output tokens at $10/M, and one search at $0.01.
    assert_eq!(usage.cost_micro_usd, 200 + 400 + 10_000);
    assert_eq!(
        d.untrusted(),
        Some("web search results from Anthropic's servers")
    );
}

#[test]
fn tp_prov_05_the_answer_lists_its_sources() {
    let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
    let out = feed(&mut d, &search_events("end_turn"));
    let EventBody::ModelResponse(response) = &out[6] else {
        panic!("model.response");
    };
    assert_eq!(
        response.text,
        format!("Let me look. He was born on April 30, 1916.\n\nSources:\n- {TITLE}: {URL}")
    );
    assert_eq!(
        d.said(),
        "Let me look. He was born on April 30, 1916.",
        "the model's own words, without the list"
    );
}

#[test]
fn tp_prov_05_sources_are_cleaned_listed_once_and_bounded() {
    let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
    let mut events = vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ];
    for n in 0..15 {
        let cited = json!({"type": "web_search_result_location",
            "url": format!("https://example.com/{n}"), "title": format!("Page\u{7}\n{n}"),
            "encrypted_index": "x", "cited_text": "y"});
        events.push(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "citations_delta", "citation": cited.clone()}}));
        events.push(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "citations_delta", "citation": cited}}));
    }
    events.push(json!({"type": "content_block_delta", "index": 0,
        "delta": {"type": "text_delta", "text": "Done."}}));
    events.push(json!({"type": "content_block_stop", "index": 0}));
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}));
    events.push(json!({"type": "message_stop"}));
    let out = feed(&mut d, &events);
    let Some(EventBody::ModelResponse(response)) =
        out.iter().find(|b| b.kind().as_str() == "model.response")
    else {
        panic!("model.response: {:?}", kinds(&out));
    };
    let lines: Vec<&str> = response
        .text
        .lines()
        .filter(|l| l.starts_with("- "))
        .collect();
    assert_eq!(lines.len(), 10, "at most ten sources");
    assert_eq!(
        lines[0], "- Page0: https://example.com/0",
        "no control characters"
    );
    assert_eq!(
        lines[9], "- Page9: https://example.com/9",
        "each once, in order"
    );
}

#[test]
fn tp_prov_06_the_reply_goes_back_exactly_as_it_came() {
    let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
    feed(&mut d, &search_events("end_turn"));
    let content = json!([
        {"type": "text", "text": "Let me look. "},
        {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
         "input": {"query": "claude shannon born"}},
        {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [result_item()]},
        {"type": "text", "text": "He was born on April 30, 1916.", "citations": [citation()]},
    ]);
    assert_eq!(Value::Array(d.content().to_vec()), content);

    let mut conversation = MessagesConversation::new("Who was Claude Shannon?");
    conversation.reply(d.content(), &[]);
    assert_eq!(
        conversation.messages(),
        [
            json!({"role": "user", "content": "Who was Claude Shannon?"}),
            json!({"role": "assistant", "content": content}),
        ],
        "with no calls to answer, no empty user message follows"
    );
}

#[test]
fn tp_prov_07_a_failed_search_is_shown_and_reads_nothing_untrusted() {
    let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "server_tool_use", "id": "srvtoolu_9", "name": "web_search", "input": {"query": "x"}}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "web_search_tool_result", "tool_use_id": "srvtoolu_9",
            "content": {"type": "web_search_tool_result_error", "error_code": "max_uses_exceeded"}}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ];
    let out = feed(&mut d, &events);
    let Some(EventBody::ToolRequested(requested)) =
        out.iter().find(|b| b.kind().as_str() == "tool.requested")
    else {
        panic!("tool.requested: {:?}", kinds(&out));
    };
    assert_eq!(
        requested.args,
        json!({"query": "x"}),
        "input that came whole in the start block"
    );
    let Some(EventBody::ToolResult(result)) =
        out.iter().find(|b| b.kind().as_str() == "tool.result")
    else {
        panic!("tool.result: {:?}", kinds(&out));
    };
    assert!(!result.ok);
    assert_eq!(result.output, json!({"error": "max_uses_exceeded"}));
    assert_eq!(d.untrusted(), None);
    assert_eq!(kinds(&out).last(), Some(&"run.completed"));
}

// ---- TP-PROV-08: what stays refused ------------------------------------------------------------

#[test]
fn tp_prov_08_other_tools_that_run_at_anthropic_are_still_refused() {
    let blocks = [
        json!({"type": "server_tool_use", "id": "srvtoolu_2", "name": "code_execution", "input": {}}),
        json!({"type": "mcp_tool_use", "id": "mcptoolu_1", "name": "notes", "input": {}}),
        json!({"type": "web_fetch_tool_result", "tool_use_id": "srvtoolu_3", "content": {}}),
    ];
    for block in blocks {
        let mut d = MessageDecoder::offering_tools(searching().price).with_web_search();
        let out = feed(
            &mut d,
            &[
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": block}),
            ],
        );
        let [EventBody::RunFailed(failed)] = out.as_slice() else {
            panic!("expected one run.failed for {block}, got {:?}", kinds(&out));
        };
        assert_eq!(failed.reason, FailureReason::ToolsNotEnabled, "{block}");
    }

    // A decoder for an agent without the search refuses it, as before (TP-TOOL-17).
    let mut d = MessageDecoder::offering_tools(searching().price);
    let out = feed(&mut d, &search_events("end_turn"));
    let failed: Vec<_> = out
        .iter()
        .filter(|b| b.kind().as_str() == "run.failed")
        .collect();
    assert_eq!(failed.len(), 1, "{:?}", kinds(&out));
    assert!(!kinds(&out).contains(&"tool.requested"));
}

// ---- TP-PROV-09: the agent loop with search ----------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

/// A tool that sends data out of the Mac.
struct Upload {
    calls: Arc<Mutex<u32>>,
}

impl Tool for Upload {
    fn name(&self) -> &str {
        "net.upload"
    }

    fn class(&self) -> ActionClass {
        ActionClass::NetWrite
    }

    fn destination(&self, _args: &Value) -> Option<String> {
        Some("example.com".into())
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(ToolSpec {
            description: "Upload a note.".into(),
            parameters: json!({"type": "object"}),
        })
    }

    fn call<'a>(&'a self, _args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        *self.calls.lock().expect("calls") += 1;
        Box::pin(async { Ok("uploaded".to_owned()) })
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

fn says(text: &str) -> RecordedResponse {
    sse(&[
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ])
}

/// A response that searches, then calls the upload tool.
fn searches_then_uploads() -> RecordedResponse {
    let mut events = search_events("tool_use");
    let end = events.split_off(events.len() - 2);
    events.push(
        json!({"type": "content_block_start", "index": 4, "content_block": {
        "type": "tool_use", "id": "toolu_1", "name": "net_upload", "input": {}}}),
    );
    events.push(json!({"type": "content_block_stop", "index": 4}));
    events.extend(end);
    sse(&events)
}

struct World {
    sup: Supervisor,
    log: Arc<MemLog>,
    desk: Arc<CheckpointDesk>,
    transport: Arc<CassetteTransport>,
    uploads: Arc<Mutex<u32>>,
}

fn world(profile: &str, config: MessagesConfig, responses: Vec<RecordedResponse>) -> World {
    let uploads = Arc::new(Mutex::new(0));
    let mut gateway = ToolGateway::new();
    gateway
        .register(Arc::new(Upload {
            calls: Arc::clone(&uploads),
        }))
        .expect("register");
    let desk = Arc::new(CheckpointDesk::default());
    let keys = MemoryStore::default();
    keys.set(Provider::Anthropic, &Secret::new(KEY).expect("key"))
        .expect("store");
    let transport = Arc::new(CassetteTransport::new(
        Cassette {
            description: "Claude searching".into(),
            responses,
        },
        1 << 20,
    ));
    let n = Arc::new(AtomicU64::new(0));
    let agent = MessagesAgent::new(
        transport.clone(),
        Arc::new(keys),
        config,
        ToolSetup {
            policy: Policy::new(Profile::from_toml(profile).expect("profile")),
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
        prompt: "Who was Claude Shannon?".into(),
    })
    .expect("deploy");
    World {
        sup,
        log,
        desk,
        transport,
        uploads,
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

    fn offered(&self, request: usize) -> Vec<String> {
        self.transport.seen()[request].body["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().expect("name").to_owned())
            .collect()
    }
}

#[tokio::test(start_paused = true)]
async fn tp_prov_09_without_the_policy_the_agent_isnt_offered_search() {
    let w = world("", searching(), vec![says("I can't search.")]);
    w.idle().await;
    assert_eq!(
        w.offered(0),
        ["net_upload"],
        "the policy turned the search off"
    );
    assert_eq!(w.kinds().last(), Some(&"run.completed"));
}

#[tokio::test(start_paused = true)]
async fn tp_prov_09_a_paused_turn_is_sent_back_and_resumed() {
    let w = world(
        ALLOWS_PROVIDER_TOOLS,
        searching(),
        vec![sse(&search_events("pause_turn")), says("Born in 1916.")],
    );
    w.idle().await;
    let seen = w.transport.seen();
    assert_eq!(
        seen.len(),
        2,
        "the paused turn is resumed once: {:?}",
        w.kinds()
    );
    assert_eq!(w.offered(0), ["net_upload", "web_search"]);
    let messages = seen[1].body["messages"]
        .as_array()
        .expect("messages")
        .clone();
    assert_eq!(
        messages.len(),
        2,
        "the prompt, then the paused reply as it came"
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][2]["content"][0], result_item());
    let completed = w.kinds().iter().filter(|k| **k == "run.completed").count();
    assert_eq!(completed, 1, "{:?}", w.kinds());
    assert_eq!(w.kinds().last(), Some(&"run.completed"));
}

#[tokio::test(start_paused = true)]
async fn tp_prov_09_after_a_search_sending_data_out_names_the_untrusted_content() {
    let w = world(
        ALLOWS_PROVIDER_TOOLS,
        searching(),
        vec![searches_then_uploads(), says("Uploaded.")],
    );
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.desk.waiting().len(),
        1,
        "the upload asks: {:?}",
        w.kinds()
    );
    assert_eq!(*w.uploads.lock().expect("uploads"), 0);
    let reason = w
        .log
        .0
        .lock()
        .expect("log")
        .iter()
        .find_map(|e| {
            let EventBody::PolicyDecided(PolicyDecided {
                decision: PolicyDecision::Ask { reason, .. },
                ..
            }) = &e.body
            else {
                return None;
            };
            Some(reason.clone())
        })
        .expect("an ask");
    assert!(
        reason.contains("untrusted content (web search results from Anthropic's servers)"),
        "{reason}"
    );
    let messages = w.transport.seen()[0].body["messages"].clone();
    assert_eq!(messages.as_array().map(Vec::len), Some(1));
}
