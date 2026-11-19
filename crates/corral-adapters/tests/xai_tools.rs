//! Grok's tools on the wire (M3 item 3, TP-TOOL-04..06): what a Responses request looks like when
//! the agent has tools, how the conversation is carried, and how the model's function calls are
//! read from the stream. Pure: no network, no driver (the agent loop that uses these is next).
//!
//! `store: false` keeps every turn off xAI's servers, so each request carries the whole
//! conversation: the prompt, what the model said, each call it made and what came back.

use corral_adapters::responses::{
    Conversation, FunctionCall, MAX_CALL_ARGUMENT_BYTES, ResponseDecoder, ResponsesConfig,
};
use corral_adapters::sse::SseEvent;
use corral_core::event::{EventBody, FailureReason};
use corral_tools::OfferedTool;
use serde_json::{Value, json};

fn config() -> ResponsesConfig {
    ResponsesConfig::xai("grok-test")
}

fn offered() -> Vec<OfferedTool> {
    vec![
        OfferedTool {
            wire_name: "fs_read".into(),
            name: "fs.read".into(),
            description: "Read a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "A path."}},
                "required": ["path"],
                "additionalProperties": false,
            }),
        },
        OfferedTool {
            wire_name: "fs_write".into(),
            name: "fs.write".into(),
            description: "Write a file.".into(),
            parameters: json!({"type": "object"}),
        },
    ]
}

fn event(data: Value) -> SseEvent {
    SseEvent {
        event: data["type"].as_str().map(str::to_owned),
        data: data.to_string(),
    }
}

fn feed(decoder: &mut ResponseDecoder, events: &[Value]) -> Vec<EventBody> {
    events
        .iter()
        .flat_map(|e| decoder.on_event(&event(e.clone())))
        .collect()
}

fn kinds(out: &[EventBody]) -> Vec<&'static str> {
    out.iter().map(|body| body.kind().as_str()).collect()
}

fn call_added(id: &str, call_id: &str, name: &str) -> Value {
    json!({"type": "response.output_item.added", "output_index": 0, "item": {
        "id": id, "type": "function_call", "call_id": call_id, "name": name,
        "arguments": "", "status": "in_progress"}})
}

fn call_done(id: &str, call_id: &str, name: &str, arguments: &str) -> Value {
    json!({"type": "response.output_item.done", "output_index": 0, "item": {
        "id": id, "type": "function_call", "call_id": call_id, "name": name,
        "arguments": arguments, "status": "completed"}})
}

fn completed() -> Value {
    json!({"type": "response.completed", "response": {"status": "completed",
        "usage": {"input_tokens": 120, "output_tokens": 30, "cost_in_usd_ticks": 4_200_000}}})
}

// ---- TP-TOOL-04: the request ---------------------------------------------------------------------

#[test]
fn tp_tool_04_a_request_offers_the_agents_tools_one_call_at_a_time() {
    let conversation = Conversation::new("Tidy my notes.");
    let body = config().turn_body(&conversation, &offered(), 4_000);
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "input",
            "max_output_tokens",
            "model",
            "parallel_tool_calls",
            "store",
            "stream",
            "tool_choice",
            "tools",
        ]
    );
    assert_eq!(body["store"], false, "nothing is kept at xAI");
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "grok-test");
    assert_eq!(body["max_output_tokens"], 4_000);
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(
        body["parallel_tool_calls"], false,
        "one call at a time, so checkpoints come one by one"
    );
    assert_eq!(
        body["tools"],
        json!([
            {"type": "function", "name": "fs_read", "description": "Read a file.",
             "parameters": offered()[0].parameters},
            {"type": "function", "name": "fs_write", "description": "Write a file.",
             "parameters": {"type": "object"}},
        ])
    );
    assert_eq!(
        body["input"],
        json!([{"role": "user", "content": "Tidy my notes."}])
    );
}

#[test]
fn tp_tool_04_with_no_tools_nothing_tool_shaped_is_sent() {
    let body = config().turn_body(&Conversation::new("Say hello."), &[], 100);
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["input", "max_output_tokens", "model", "store", "stream"]
    );
}

// ---- TP-TOOL-05: the conversation ----------------------------------------------------------------

#[test]
fn tp_tool_05_the_conversation_carries_every_turn_in_order() {
    let mut conversation = Conversation::new("Tidy my notes.");
    conversation.said("Let me look first.");
    let call = FunctionCall {
        call_id: "call_1".into(),
        name: "fs_read".into(),
        arguments: r#"{"path":"notes.md"}"#.into(),
    };
    conversation.called(&call, "my notes");
    conversation.said("");
    assert_eq!(
        conversation.items(),
        [
            json!({"role": "user", "content": "Tidy my notes."}),
            json!({"role": "assistant", "content": "Let me look first."}),
            json!({"type": "function_call", "call_id": "call_1", "name": "fs_read",
                   "arguments": "{\"path\":\"notes.md\"}"}),
            json!({"type": "function_call_output", "call_id": "call_1", "output": "my notes"}),
        ],
        "nothing for an empty reply; the call goes back exactly as the model made it"
    );
    let body = config().turn_body(&conversation, &offered(), 100);
    assert_eq!(body["input"], json!(conversation.items()));
}

#[test]
fn tp_tool_05_the_budget_sees_the_whole_conversation_and_the_tools() {
    let c = config();
    let short = Conversation::new("Hi.");
    let mut long = short.clone();
    long.called(
        &FunctionCall {
            call_id: "call_1".into(),
            name: "fs_read".into(),
            arguments: "{}".into(),
        },
        &"x".repeat(10_000),
    );
    let bare = c.turn_estimate(&short, &[]);
    let with_tools = c.turn_estimate(&short, &offered());
    let after_a_call = c.turn_estimate(&long, &offered());
    assert!(with_tools.input_tokens > bare.input_tokens, "tools count");
    assert!(
        after_a_call.input_tokens >= with_tools.input_tokens + 10_000,
        "a tool's output counts, one token per byte at most"
    );
    assert_eq!(after_a_call.price, c.price);
    assert_eq!(after_a_call.max_output_tokens, c.max_output_tokens);
}

// ---- TP-TOOL-06: reading the model's calls ------------------------------------------------------------

#[test]
fn tp_tool_06_a_function_call_is_read_whole_and_the_run_goes_on() {
    let mut d = ResponseDecoder::offering_tools();
    let out = feed(
        &mut d,
        &[
            json!({"type": "response.output_item.added", "item": {"type": "reasoning"}}),
            call_added("fc_1", "call_1", "fs_write"),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "delta": "{\"path\":"}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "delta": "\"todo.md\",\"content\":\"buy milk\"}"}),
            call_done(
                "fc_1",
                "call_1",
                "fs_write",
                r#"{"path":"todo.md","content":"buy milk"}"#,
            ),
            completed(),
        ],
    );
    assert_eq!(
        kinds(&out),
        ["model.response", "usage.reported"],
        "no run.completed: the agent loop runs the call and asks again"
    );
    assert!(d.is_finished(), "this response is over");
    assert_eq!(
        d.take_calls(),
        [FunctionCall {
            call_id: "call_1".into(),
            name: "fs_write".into(),
            arguments: r#"{"path":"todo.md","content":"buy milk"}"#.into(),
        }]
    );
    assert!(d.take_calls().is_empty(), "taken once");
}

#[test]
fn tp_tool_06_text_with_no_call_completes_the_run_as_before() {
    let mut d = ResponseDecoder::offering_tools();
    let out = feed(
        &mut d,
        &[
            json!({"type": "response.output_text.delta", "delta": "All tidy."}),
            completed(),
        ],
    );
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

#[test]
fn tp_tool_06_every_call_in_a_response_is_kept_in_order() {
    let mut d = ResponseDecoder::offering_tools();
    feed(
        &mut d,
        &[
            call_done("fc_1", "call_1", "fs_read", r#"{"path":"a.md"}"#),
            call_done("fc_2", "call_2", "fs_read", r#"{"path":"b.md"}"#),
            completed(),
        ],
    );
    let ids: Vec<String> = d.take_calls().into_iter().map(|c| c.call_id).collect();
    assert_eq!(ids, ["call_1", "call_2"]);
}

fn single_failure(out: &[EventBody]) -> (FailureReason, String) {
    match out {
        [EventBody::RunFailed(f)] => (f.reason, f.message.clone()),
        other => panic!("expected one run.failed, got {:?}", kinds(other)),
    }
}

#[test]
fn tp_tool_06_a_tool_that_runs_at_xai_is_still_refused() {
    let mut d = ResponseDecoder::offering_tools();
    let out = feed(
        &mut d,
        &[json!({"type": "response.output_item.added", "item": {
            "type": "web_search_call", "id": "ws_1", "status": "in_progress"}})],
    );
    let (reason, message) = single_failure(&out);
    assert_eq!(reason, FailureReason::ToolsNotEnabled);
    assert!(message.contains("web_search_call"), "{message}");
    assert!(d.take_calls().is_empty());
}

#[test]
fn tp_tool_06_a_broken_call_fails_the_run_and_nothing_is_kept() {
    let too_big = format!(r#"{{"content":"{}"}}"#, "x".repeat(MAX_CALL_ARGUMENT_BYTES));
    for done in [
        call_done("fc_1", "", "fs_read", "{}"),
        call_done("fc_1", "call_1", "", "{}"),
        call_done("fc_1", "call_1", "fs_write", &too_big),
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "call_id": "call_1", "name": "fs_read"}}),
    ] {
        let mut d = ResponseDecoder::offering_tools();
        let out = feed(&mut d, &[done.clone(), completed()]);
        let (reason, _) = single_failure(&out);
        assert_eq!(reason, FailureReason::Adapter, "{done}");
        assert!(d.is_finished());
        assert!(d.take_calls().is_empty(), "{done}");
    }
}

#[test]
fn tp_tool_06_without_tools_a_call_fails_the_run_as_in_the_skeleton() {
    let mut d = ResponseDecoder::default();
    let out = feed(&mut d, &[call_added("fc_1", "call_1", "send_email")]);
    let (reason, message) = single_failure(&out);
    assert_eq!(reason, FailureReason::ToolsNotEnabled);
    assert!(message.contains("send_email"), "{message}");
}

#[test]
fn tp_tool_06_arguments_right_at_the_limit_are_kept() {
    let padding = MAX_CALL_ARGUMENT_BYTES - r#"{"content":""}"#.len();
    let at_limit = format!(r#"{{"content":"{}"}}"#, "x".repeat(padding));
    assert_eq!(at_limit.len(), MAX_CALL_ARGUMENT_BYTES);
    let mut d = ResponseDecoder::offering_tools();
    let out = feed(
        &mut d,
        &[
            call_done("fc_1", "call_1", "fs_write", &at_limit),
            completed(),
        ],
    );
    assert_eq!(kinds(&out), ["model.response", "usage.reported"]);
    assert_eq!(d.take_calls().len(), 1);
}

#[test]
fn tp_tool_06_without_tools_a_finished_call_alone_still_fails_the_run() {
    let mut d = ResponseDecoder::default();
    let out = feed(
        &mut d,
        &[call_done("fc_1", "call_1", "send_email", "{}"), completed()],
    );
    let (reason, message) = single_failure(&out);
    assert_eq!(reason, FailureReason::ToolsNotEnabled);
    assert!(message.contains("send_email"), "{message}");
    assert!(d.take_calls().is_empty());
}
