//! The OpenAI Responses protocol (`POST /v1/responses`, streamed), as xAI serves it for Grok.
//!
//! - **No tools unless offered** (TP-SKEL-02): the plain request carries no `tools`,
//!   `tool_choice` or provider-side tools, and `store: false` keeps the conversation off xAI's
//!   servers. If the model tries to call a tool anyway, the run fails with
//!   [`FailureReason::ToolsNotEnabled`] and nothing runs.
//! - **With tools** (M3, TP-TOOL-04..06): [`ResponsesConfig::turn_body`] offers the agent's tools
//!   as function tools, one call at a time (`parallel_tool_calls: false`), and carries the whole
//!   [`Conversation`] as input items, since nothing is stored at xAI. A decoder made with
//!   [`ResponseDecoder::offering_tools`] reads each finished function call
//!   ([`ResponseDecoder::take_calls`]) and leaves the run going for the agent loop. Tools that run
//!   on xAI's servers are still refused.
//! - **The key is read at request time** from the [`SecretStore`] (TP-SKEL-01); without one the
//!   run fails with "Add your xAI key" before anything is sent.
//! - **Events:** `model.request` once per run, a `model.delta` per text chunk, then
//!   `model.response`, `usage.reported` and `run.completed`; or one `run.failed` (TP-ADP-01).
//! - **Errors are typed** (TP-ADP-04): a rejected key, a persistent rate limit, a server error or
//!   a failed response is a provider failure; a stream Corral can't read is an adapter failure.
//!   Retries and Stop are shared with every streamed protocol ([`crate::stream`]).
//! - **Usage** (TP-ADP-05): the provider's billed cost (`cost_in_usd_ticks`, 10^-10 USD) is
//!   reported in micro-USD, rounded up so spend is never under-counted.
//! - **The hard spend cap** (SK-5, TP-BUD-01/05): before anything is sent, the request goes to
//!   the budget meter through [`EventSink::request_model`] with its worst case: the prompt
//!   bounded at one token per byte plus overhead, at [`ResponsesConfig::price`]. Over the cap,
//!   the core fails the run and nothing is sent; otherwise the request carries the
//!   `max_output_tokens` the meter allows. Retries reuse the one admission: a rate-limited or
//!   unavailable attempt isn't billed.
//! - **Stop** (TP-ADP-03): the whole request runs inside a `select!` on the cancel token, so
//!   Stop drops the connection at once.

use crate::agent::ToolProtocol;
use crate::sse::SseEvent;
use crate::stream::{Protocol, StreamDecoder, StreamDriver, clean};
use crate::transport::KeyHeader;
use corral_core::budget::{Price, RequestEstimate, input_token_bound};
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelResponse, RunCompleted, RunFailed, UsageReported,
};
use corral_secrets::Provider;
use corral_tools::OfferedTool;
use serde_json::{Value, json};
use std::time::Duration;

/// Where and how to call a Responses API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesConfig {
    /// Whose key to use, and the name in messages.
    pub provider: Provider,
    /// API base, e.g. `https://api.x.ai/v1`.
    pub base_url: String,
    /// Model ID (configuration, never hard-coded in callers).
    pub model: String,
    /// Upper bound on output tokens per request, reasoning included. The budget meter may lower
    /// it for a request; it never raises it.
    pub max_output_tokens: u32,
    /// The model's price, for the budget meter's worst case before each request.
    pub price: Price,
    /// Attempts in all for a rate-limited or unavailable request.
    pub max_attempts: u32,
    /// Fail the run if the stream sends nothing for this long.
    pub idle_timeout: Duration,
}

impl ResponsesConfig {
    /// xAI's API with the given model, priced at grok-4.7's list price: $2 per million input
    /// tokens and $6 per million output tokens, doubling to $4 and $12 for every token of a
    /// request whose prompt has 200,000 tokens or more (docs.x.ai pricing, checked 2026-11-24).
    /// Set [`Self::price`] for another model.
    pub fn xai(model: impl Into<String>) -> Self {
        Self {
            provider: Provider::Xai,
            base_url: "https://api.x.ai/v1".into(),
            model: model.into(),
            max_output_tokens: 16_384,
            price: Price::per_million_tokens(2_000_000, 6_000_000)
                .with_long_prompt(200_000, 4_000_000, 12_000_000),
            max_attempts: 3,
            idle_timeout: Duration::from_secs(300),
        }
    }

    /// The endpoint.
    pub fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    /// The request body at the configured output ceiling: the prompt and nothing that could run
    /// anything (TP-SKEL-02).
    pub fn request_body(&self, prompt: &str) -> Value {
        self.request_body_limited(prompt, self.max_output_tokens)
    }

    /// The request body with the output limit the budget meter allowed.
    pub fn request_body_limited(&self, prompt: &str, max_output_tokens: u32) -> Value {
        json!({
            "model": self.model,
            "input": prompt,
            "stream": true,
            "store": false,
            "max_output_tokens": max_output_tokens,
        })
    }

    /// The most this request could cost, for the budget meter.
    pub fn estimate(&self, prompt: &str) -> RequestEstimate {
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(prompt),
            // No provider-side tools in the walking skeleton (TP-SKEL-02).
            tool_fees_micro_usd: 0,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

pub use crate::agent::{FunctionCall, MAX_CALL_ARGUMENT_BYTES};

/// Everything said so far in a run, as Responses input items. Nothing is stored at xAI, so each
/// request carries it all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    items: Vec<Value>,
}

impl Conversation {
    /// A conversation that starts with the user's prompt.
    pub fn new(prompt: &str) -> Self {
        Self {
            items: vec![json!({"role": "user", "content": prompt})],
        }
    }

    /// The model's reply text. An empty reply adds nothing.
    pub fn said(&mut self, text: &str) {
        if !text.is_empty() {
            self.items
                .push(json!({"role": "assistant", "content": text}));
        }
    }

    /// A call the model made, exactly as it made it, and what came back.
    pub fn called(&mut self, call: &FunctionCall, output: &str) {
        self.items.push(json!({
            "type": "function_call",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": call.arguments,
        }));
        self.items.push(json!({
            "type": "function_call_output",
            "call_id": call.call_id,
            "output": output,
        }));
    }

    /// The input items, in order.
    pub fn items(&self) -> &[Value] {
        &self.items
    }
}

impl ToolProtocol for ResponsesConfig {
    type Conversation = Conversation;
    type ToolDecoder = ResponseDecoder;
    type Said = String;

    fn conversation(&self, prompt: &str) -> Conversation {
        Conversation::new(prompt)
    }

    fn turn_body(&self, conversation: &Conversation, tools: &[OfferedTool], max: u32) -> Value {
        ResponsesConfig::turn_body(self, conversation, tools, max)
    }

    fn turn_estimate(&self, conversation: &Conversation, tools: &[OfferedTool]) -> RequestEstimate {
        ResponsesConfig::turn_estimate(self, conversation, tools)
    }

    fn tool_decoder(&self) -> ResponseDecoder {
        ResponseDecoder::offering_tools()
    }

    fn take_turn(decoder: &mut ResponseDecoder) -> (String, Vec<FunctionCall>) {
        (decoder.said().to_owned(), decoder.take_calls())
    }

    fn record(conversation: &mut Conversation, said: &String, results: &[(FunctionCall, String)]) {
        conversation.said(said);
        for (call, output) in results {
            conversation.called(call, output);
        }
    }
}

/// The tools as Responses function tools.
fn function_tools(tools: &[OfferedTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.wire_name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

impl ResponsesConfig {
    /// One turn's request: the whole conversation, the agent's tools (none means nothing
    /// tool-shaped is sent), and the output limit the budget meter allowed.
    pub fn turn_body(
        &self,
        conversation: &Conversation,
        tools: &[OfferedTool],
        max_output_tokens: u32,
    ) -> Value {
        let mut body = json!({
            "model": self.model,
            "input": conversation.items(),
            "stream": true,
            "store": false,
            "max_output_tokens": max_output_tokens,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(function_tools(tools));
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        }
        body
    }

    /// The most one turn could cost: the conversation and the tools' descriptions count as input.
    pub fn turn_estimate(
        &self,
        conversation: &Conversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate {
        let input = json!({"input": conversation.items(), "tools": function_tools(tools)});
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(&input.to_string()),
            // Provider-side tools stay off (S3, R3).
            tool_fees_micro_usd: 0,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

/// A [`RunDriver`] that runs a prompt through a Responses API.
pub type ResponsesDriver = StreamDriver<ResponsesConfig>;

impl Protocol for ResponsesConfig {
    type Decoder = ResponseDecoder;

    fn provider(&self) -> Provider {
        self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn estimate(&self, prompt: &str) -> RequestEstimate {
        ResponsesConfig::estimate(self, prompt)
    }

    fn url(&self) -> String {
        ResponsesConfig::url(self)
    }

    fn body(&self, prompt: &str, max_output_tokens: u32) -> Value {
        self.request_body_limited(prompt, max_output_tokens)
    }

    fn key_header(&self) -> KeyHeader {
        KeyHeader::Bearer
    }

    fn headers(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    fn decoder(&self) -> ResponseDecoder {
        ResponseDecoder::default()
    }
}

impl StreamDecoder for ResponseDecoder {
    fn on_event(&mut self, event: &SseEvent) -> Vec<EventBody> {
        ResponseDecoder::on_event(self, event)
    }

    fn is_finished(&self) -> bool {
        ResponseDecoder::is_finished(self)
    }
}

/// Turns Responses stream events into Corral events. Pure, so the recorded cassettes and the
/// recorder check it without a network.
#[derive(Debug, Default)]
pub struct ResponseDecoder {
    text: String,
    finished: bool,
    /// The agent was offered tools, so its function calls are read rather than refused.
    tools: bool,
    calls: Vec<FunctionCall>,
    /// The reply's whole text, once the response is over.
    said: String,
}

impl ResponseDecoder {
    /// A decoder for a request that offered tools: function calls are kept for the agent loop.
    pub fn offering_tools() -> Self {
        Self {
            tools: true,
            ..Self::default()
        }
    }

    /// The reply's whole text, once the response is over (empty before).
    pub fn said(&self) -> &str {
        &self.said
    }

    /// The function calls the response made, in order. Each is handed out once.
    pub fn take_calls(&mut self) -> Vec<FunctionCall> {
        std::mem::take(&mut self.calls)
    }

    /// True once a terminal event (completed or failed) has been produced.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The Corral events for one stream event. Unknown event types are ignored, so new kinds
    /// of stream events (reasoning summaries, annotations) don't break runs.
    pub fn on_event(&mut self, event: &SseEvent) -> Vec<EventBody> {
        if self.finished || event.data == "[DONE]" {
            return Vec::new();
        }
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return self.fail(
                FailureReason::Adapter,
                "the provider sent an event that isn't JSON".into(),
            );
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or_default();
        match kind {
            "response.output_text.delta" => {
                let delta = data
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    return Vec::new();
                }
                self.text.push_str(delta);
                vec![EventBody::ModelDelta(ModelDelta {
                    text: delta.to_owned(),
                })]
            }
            "response.output_item.added" => {
                let item = data.get("item");
                let item_type = item
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                match item_type {
                    "message" | "reasoning" => Vec::new(),
                    // Read whole when it's done.
                    "function_call" if self.tools => Vec::new(),
                    other => {
                        let tool = item
                            .and_then(|i| i.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or(other);
                        self.refuse_tool(tool)
                    }
                }
            }
            "response.output_item.done" => match data.get("item") {
                Some(item) if item.get("type").and_then(Value::as_str) == Some("function_call") => {
                    if self.tools {
                        self.read_call(item)
                    } else {
                        let tool = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("function_call");
                        self.refuse_tool(tool)
                    }
                }
                _ => Vec::new(),
            },
            "response.completed" | "response.incomplete" => {
                let response = data.get("response");
                let finish_reason = response
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        response
                            .and_then(|r| r.get("status"))
                            .and_then(Value::as_str)
                    })
                    .map(clean);
                self.said = std::mem::take(&mut self.text);
                let mut out = vec![EventBody::ModelResponse(ModelResponse {
                    text: self.said.clone(),
                    finish_reason,
                })];
                if let Some(usage) = response.and_then(|r| r.get("usage")) {
                    out.push(EventBody::UsageReported(usage_from(usage)));
                }
                // With calls to make, the agent loop runs them and asks again.
                if self.calls.is_empty() {
                    out.push(EventBody::RunCompleted(RunCompleted { summary: None }));
                }
                self.finished = true;
                out
            }
            "response.failed" => {
                let message = data
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(clean)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the provider reported that the response failed".into());
                self.fail(FailureReason::Provider, message)
            }
            "error" => {
                let message = data
                    .get("message")
                    .and_then(Value::as_str)
                    .map(clean)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the provider reported an error".into());
                self.fail(FailureReason::Provider, message)
            }
            _ => Vec::new(),
        }
    }

    /// Fails the run over a tool the agent wasn't given. Nothing ran.
    fn refuse_tool(&mut self, tool: &str) -> Vec<EventBody> {
        let message = if self.tools {
            format!(
                "The model asked to use a tool Corral didn't give it ({}). Nothing ran.",
                clean(tool)
            )
        } else {
            format!(
                "The model asked to use a tool ({}), but this agent has no tools yet. Nothing ran.",
                clean(tool)
            )
        };
        self.fail(FailureReason::ToolsNotEnabled, message)
    }

    /// Keeps a finished function call, or fails the run if it can't be used.
    fn read_call(&mut self, item: &Value) -> Vec<EventBody> {
        let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default();
        let (call_id, name) = (text("call_id"), text("name"));
        let Some(arguments) = item.get("arguments").and_then(Value::as_str) else {
            return self.fail(
                FailureReason::Adapter,
                "the model made a tool call with no arguments".into(),
            );
        };
        if call_id.is_empty() || name.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the model made a tool call without a name or an ID".into(),
            );
        }
        if arguments.len() > MAX_CALL_ARGUMENT_BYTES {
            return self.fail(
                FailureReason::Adapter,
                format!(
                    "the model made a tool call with {} KB of arguments; the most is {} KB",
                    arguments.len() / 1024,
                    MAX_CALL_ARGUMENT_BYTES / 1024
                ),
            );
        }
        self.calls.push(FunctionCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        });
        Vec::new()
    }

    fn fail(&mut self, reason: FailureReason, message: String) -> Vec<EventBody> {
        self.finished = true;
        self.calls.clear();
        vec![EventBody::RunFailed(RunFailed { reason, message })]
    }
}

/// `usage` → tokens and the billed cost in micro-USD (10^4 ticks), rounded up.
fn usage_from(usage: &Value) -> UsageReported {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    UsageReported {
        input_tokens: field("input_tokens"),
        output_tokens: field("output_tokens"),
        cost_micro_usd: field("cost_in_usd_ticks").div_ceil(10_000),
    }
}
