//! Anthropic's Messages protocol (`POST /v1/messages`, streamed), for Claude (SK-7), with the same
//! contract as [`crate::responses`]:
//!
//! - **No tools unless offered** (TP-SKEL-02): the plain body is the model, `max_tokens`,
//!   `stream` and one user message; no `tools`, `tool_choice`, `mcp_servers` or system prompt. A
//!   content block that is any kind of tool use (`tool_use`, `server_tool_use`, `mcp_tool_use`…)
//!   fails the run with [`FailureReason::ToolsNotEnabled`]. Thinking blocks are read and not
//!   shown; other block kinds Anthropic adds later are ignored, as its versioning policy asks.
//! - **With tools** (TP-TOOL-16..18): [`MessagesConfig::turn_body`] offers the agent's tools
//!   (`name`, `description`, `input_schema`), one call at a time
//!   (`disable_parallel_tool_use`), and carries the whole [`MessagesConversation`]: each turn
//!   goes back as an assistant message with its text and `tool_use` blocks, then a user message
//!   with a `tool_result` for each. [`MessageDecoder::offering_tools`] assembles each `tool_use`
//!   block from its streamed JSON pieces and leaves the run going for the agent loop. Tools that
//!   run at Anthropic (`server_tool_use`, `mcp_tool_use`) are still refused.
//! - **The key** is read at request time and sent in `x-api-key`, with
//!   `anthropic-version: 2023-06-01`.
//! - **Usage** (TP-ADP-05): input is `input_tokens` plus cache writes and cache reads, all priced
//!   at the full input rate; output is `message_delta`'s running total. Anthropic reports no
//!   cost, so it's computed from tokens at [`MessagesConfig::price`], rounded up.
//! - **The hard spend cap** (TP-BUD-01, 05): `max_tokens` is what the budget meter allows, never
//!   more than [`MessagesConfig::max_output_tokens`].
//! - **Errors** (TP-ADP-04): HTTP errors as in [`crate::stream`] (429, 503 and 529 retried); an
//!   `error` event mid-stream is a provider failure with Anthropic's message.

use crate::agent::{FunctionCall, MAX_CALL_ARGUMENT_BYTES, ToolProtocol};
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
use std::collections::BTreeMap;
use std::time::Duration;

/// The `anthropic-version` Corral speaks.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Where and how to call a Messages API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagesConfig {
    /// Whose key to use, and the name in messages.
    pub provider: Provider,
    /// API base, e.g. `https://api.anthropic.com/v1`.
    pub base_url: String,
    /// Model ID (configuration, never hard-coded in callers).
    pub model: String,
    /// Upper bound on output tokens per request, thinking included. The budget meter may lower
    /// it for a request; it never raises it.
    pub max_output_tokens: u32,
    /// The model's price, for the budget meter and the reported cost.
    pub price: Price,
    /// Attempts in all for a rate-limited or overloaded request.
    pub max_attempts: u32,
    /// Fail the run if the stream sends nothing for this long.
    pub idle_timeout: Duration,
}

impl MessagesConfig {
    /// Anthropic's API with the given model, priced at Claude Sonnet 5.5's list price ($2 per
    /// million input tokens, $10 per million output tokens, checked November 2026). Set
    /// [`Self::price`] for another model; the versioned price table replaces this in M2.
    pub fn anthropic(model: impl Into<String>) -> Self {
        Self {
            provider: Provider::Anthropic,
            base_url: "https://api.anthropic.com/v1".into(),
            model: model.into(),
            max_output_tokens: 16_384,
            price: Price::per_million_tokens(2_000_000, 10_000_000),
            max_attempts: 3,
            idle_timeout: Duration::from_secs(300),
        }
    }

    /// The endpoint.
    pub fn url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }

    /// The request body with the output limit the budget meter allowed: the prompt and nothing
    /// that could run anything (TP-SKEL-02).
    pub fn request_body_limited(&self, prompt: &str, max_tokens: u32) -> Value {
        json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": [{"role": "user", "content": prompt}],
        })
    }

    /// The most this request could cost, for the budget meter.
    pub fn estimate(&self, prompt: &str) -> RequestEstimate {
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(prompt),
            // No server tools are sent (TP-SKEL-02), so none can be billed.
            tool_fees_micro_usd: 0,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

/// Everything said so far in a run, as Messages API messages. Nothing is kept at Anthropic, so
/// each request carries it all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagesConversation {
    messages: Vec<Value>,
}

impl MessagesConversation {
    /// A conversation that starts with the user's prompt.
    pub fn new(prompt: &str) -> Self {
        Self {
            messages: vec![json!({"role": "user", "content": prompt})],
        }
    }

    /// A turn: the model's text and its `tool_use` blocks, then a `tool_result` for each. Input
    /// that isn't a JSON object goes back as `{}`, since Anthropic accepts nothing else.
    pub fn turn(&mut self, said: &str, results: &[(FunctionCall, String)]) {
        let mut asked = Vec::with_capacity(results.len() + 1);
        if !said.is_empty() {
            asked.push(json!({"type": "text", "text": said}));
        }
        let mut answered = Vec::with_capacity(results.len());
        for (call, output) in results {
            let input = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(object @ Value::Object(_)) => object,
                Ok(_) | Err(_) => json!({}),
            };
            asked.push(json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": input,
            }));
            answered.push(json!({
                "type": "tool_result",
                "tool_use_id": call.call_id,
                "content": output,
            }));
        }
        self.messages
            .push(json!({"role": "assistant", "content": asked}));
        self.messages
            .push(json!({"role": "user", "content": answered}));
    }

    /// The messages, in order.
    pub fn messages(&self) -> &[Value] {
        &self.messages
    }
}

/// The tools as Messages API tools.
fn messages_tools(tools: &[OfferedTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.wire_name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

impl MessagesConfig {
    /// One turn's request: the whole conversation, the agent's tools (none means nothing
    /// tool-shaped is sent), and the output limit the budget meter allowed.
    pub fn turn_body(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
        max_tokens: u32,
    ) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": conversation.messages(),
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(messages_tools(tools));
            body["tool_choice"] = json!({"type": "auto", "disable_parallel_tool_use": true});
        }
        body
    }

    /// The most one turn could cost: the conversation and the tools' descriptions count as input.
    pub fn turn_estimate(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate {
        let input = json!({"messages": conversation.messages(), "tools": messages_tools(tools)});
        RequestEstimate {
            price: self.price,
            input_tokens: input_token_bound(&input.to_string()),
            // No server tools are sent, so none can be billed.
            tool_fees_micro_usd: 0,
            max_output_tokens: self.max_output_tokens,
        }
    }
}

impl ToolProtocol for MessagesConfig {
    type Conversation = MessagesConversation;
    type ToolDecoder = MessageDecoder;

    fn conversation(&self, prompt: &str) -> MessagesConversation {
        MessagesConversation::new(prompt)
    }

    fn turn_body(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
        max: u32,
    ) -> Value {
        MessagesConfig::turn_body(self, conversation, tools, max)
    }

    fn turn_estimate(
        &self,
        conversation: &MessagesConversation,
        tools: &[OfferedTool],
    ) -> RequestEstimate {
        MessagesConfig::turn_estimate(self, conversation, tools)
    }

    fn tool_decoder(&self) -> MessageDecoder {
        MessageDecoder::offering_tools(self.price)
    }

    fn take_turn(decoder: &mut MessageDecoder) -> (String, Vec<FunctionCall>) {
        (decoder.said().to_owned(), decoder.take_calls())
    }

    fn record(
        conversation: &mut MessagesConversation,
        said: &str,
        results: &[(FunctionCall, String)],
    ) {
        conversation.turn(said, results);
    }
}

/// A [`corral_core::supervisor::RunDriver`] that runs a prompt through a Messages API.
pub type MessagesDriver = StreamDriver<MessagesConfig>;

impl Protocol for MessagesConfig {
    type Decoder = MessageDecoder;

    fn provider(&self) -> Provider {
        self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn estimate(&self, prompt: &str) -> RequestEstimate {
        MessagesConfig::estimate(self, prompt)
    }

    fn url(&self) -> String {
        MessagesConfig::url(self)
    }

    fn body(&self, prompt: &str, max_output_tokens: u32) -> Value {
        self.request_body_limited(prompt, max_output_tokens)
    }

    fn key_header(&self) -> KeyHeader {
        KeyHeader::Named("x-api-key")
    }

    fn headers(&self) -> &'static [(&'static str, &'static str)] {
        &[("anthropic-version", ANTHROPIC_VERSION)]
    }

    fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    fn decoder(&self) -> MessageDecoder {
        MessageDecoder::new(self.price)
    }
}

/// Turns Messages stream events into Corral events. Pure, so cassettes check it without a
/// network.
#[derive(Debug)]
pub struct MessageDecoder {
    price: Price,
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
    finished: bool,
    /// The agent was offered tools, so its `tool_use` blocks are read rather than refused.
    tools: bool,
    /// `tool_use` blocks still streaming, by block index.
    open: BTreeMap<u64, FunctionCall>,
    calls: Vec<FunctionCall>,
    /// The reply's whole text, once the response is over.
    said: String,
}

impl MessageDecoder {
    /// A decoder that prices usage at `price`.
    pub fn new(price: Price) -> Self {
        Self {
            price,
            text: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            finished: false,
            tools: false,
            open: BTreeMap::new(),
            calls: Vec::new(),
            said: String::new(),
        }
    }

    /// A decoder for a request that offered tools: `tool_use` blocks are kept for the loop.
    pub fn offering_tools(price: Price) -> Self {
        Self {
            tools: true,
            ..Self::new(price)
        }
    }

    /// The reply's whole text, once the response is over (empty before).
    pub fn said(&self) -> &str {
        &self.said
    }

    /// The tool calls the response made, in order. Each is handed out once.
    pub fn take_calls(&mut self) -> Vec<FunctionCall> {
        std::mem::take(&mut self.calls)
    }

    fn fail(&mut self, reason: FailureReason, message: String) -> Vec<EventBody> {
        self.finished = true;
        self.open.clear();
        self.calls.clear();
        vec![EventBody::RunFailed(RunFailed { reason, message })]
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

    /// Opens a `tool_use` block, or fails the run if it can't be used.
    fn open_call(&mut self, index: u64, block: Option<&Value>) -> Vec<EventBody> {
        let text = |key: &str| {
            block
                .and_then(|b| b.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (call_id, name) = (text("id"), text("name"));
        if call_id.is_empty() || name.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the model made a tool call without a name or an ID".into(),
            );
        }
        self.open.insert(
            index,
            FunctionCall {
                call_id,
                name,
                arguments: String::new(),
            },
        );
        Vec::new()
    }

    /// Takes the larger of what's known and what `usage` says: Anthropic's counts are running
    /// totals, so a later, smaller number is never an undo.
    fn note_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        let input = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .map(field);
        if input.iter().any(Option::is_some) {
            let total = input
                .iter()
                .flatten()
                .fold(0_u64, |sum, n| sum.saturating_add(*n));
            self.input_tokens = self.input_tokens.max(total);
        }
        if let Some(output) = field("output_tokens") {
            self.output_tokens = self.output_tokens.max(output);
        }
    }

    fn finish(&mut self) -> Vec<EventBody> {
        self.finished = true;
        let cost = self
            .price
            .input_cost(self.input_tokens)
            .saturating_add(self.price.output_cost(self.output_tokens));
        if !self.open.is_empty() {
            return self.fail(
                FailureReason::Adapter,
                "the response ended in the middle of a tool call".into(),
            );
        }
        self.said = std::mem::take(&mut self.text);
        let mut out = vec![
            EventBody::ModelResponse(ModelResponse {
                text: self.said.clone(),
                finish_reason: self.stop_reason.take(),
            }),
            EventBody::UsageReported(UsageReported {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cost_micro_usd: cost,
            }),
        ];
        // With calls to make, the agent loop runs them and asks again.
        if self.calls.is_empty() {
            out.push(EventBody::RunCompleted(RunCompleted { summary: None }));
        }
        out
    }
}

impl StreamDecoder for MessageDecoder {
    fn is_finished(&self) -> bool {
        self.finished
    }

    fn on_event(&mut self, event: &SseEvent) -> Vec<EventBody> {
        if self.finished {
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
            "message_start" => {
                self.note_usage(data.get("message").and_then(|m| m.get("usage")));
                Vec::new()
            }
            "content_block_start" => {
                let block = data.get("content_block");
                let block_type = block
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if !block_type.contains("tool") {
                    // Text, thinking, redacted thinking, and kinds added later.
                    return Vec::new();
                }
                if self.tools && block_type == "tool_use" {
                    let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                    return self.open_call(index, block);
                }
                let tool = block
                    .and_then(|b| b.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(block_type)
                    .to_owned();
                self.refuse_tool(&tool)
            }
            "content_block_stop" => {
                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(mut call) = self.open.remove(&index) {
                    if call.arguments.trim().is_empty() {
                        call.arguments = "{}".into();
                    }
                    self.calls.push(call);
                }
                Vec::new()
            }
            "content_block_delta" => {
                let delta = data.get("delta");
                let delta_type = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta_type == "input_json_delta" {
                    let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let piece = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Some(call) = self.open.get_mut(&index) else {
                        return Vec::new();
                    };
                    call.arguments.push_str(piece);
                    if call.arguments.len() > MAX_CALL_ARGUMENT_BYTES {
                        let size = call.arguments.len() / 1024;
                        return self.fail(
                            FailureReason::Adapter,
                            format!(
                                "the model made a tool call with {size} KB of arguments; the most \
                                 is {} KB",
                                MAX_CALL_ARGUMENT_BYTES / 1024
                            ),
                        );
                    }
                    return Vec::new();
                }
                if delta_type != "text_delta" {
                    // Thinking, signatures and citations aren't the answer.
                    return Vec::new();
                }
                let text = delta
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_empty() {
                    return Vec::new();
                }
                self.text.push_str(text);
                vec![EventBody::ModelDelta(ModelDelta {
                    text: text.to_owned(),
                })]
            }
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(clean(reason));
                }
                self.note_usage(data.get("usage"));
                Vec::new()
            }
            "message_stop" => self.finish(),
            "error" => {
                let message = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(clean)
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "the provider reported an error".into());
                self.fail(FailureReason::Provider, message)
            }
            // ping, and event kinds added later.
            _ => Vec::new(),
        }
    }
}
