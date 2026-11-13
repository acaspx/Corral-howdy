//! Anthropic's Messages protocol (`POST /v1/messages`, streamed), for Claude (SK-7). Text only,
//! no tools, the same contract as [`crate::responses`]:
//!
//! - **No tools** (TP-SKEL-02): the body is the model, `max_tokens`, `stream` and one user
//!   message; no `tools`, `tool_choice`, `mcp_servers` or system prompt. A content block that is
//!   any kind of tool use (`tool_use`, `server_tool_use`, `mcp_tool_use`…) fails the run with
//!   [`FailureReason::ToolsNotEnabled`]. Thinking blocks are read and not shown; other block
//!   kinds Anthropic adds later are ignored, as its versioning policy asks.
//! - **The key** is read at request time and sent in `x-api-key`, with
//!   `anthropic-version: 2023-06-01`.
//! - **Usage** (TP-ADP-05): input is `input_tokens` plus cache writes and cache reads, all priced
//!   at the full input rate; output is `message_delta`'s running total. Anthropic reports no
//!   cost, so it's computed from tokens at [`MessagesConfig::price`], rounded up.
//! - **The hard spend cap** (TP-BUD-01, 05): `max_tokens` is what the budget meter allows, never
//!   more than [`MessagesConfig::max_output_tokens`].
//! - **Errors** (TP-ADP-04): HTTP errors as in [`crate::stream`] (429, 503 and 529 retried); an
//!   `error` event mid-stream is a provider failure with Anthropic's message.

use crate::sse::SseEvent;
use crate::stream::{Protocol, StreamDecoder, StreamDriver, clean};
use crate::transport::KeyHeader;
use corral_core::budget::{Price, RequestEstimate, input_token_bound};
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelResponse, RunCompleted, RunFailed, UsageReported,
};
use corral_secrets::Provider;
use serde_json::{Value, json};
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
        }
    }

    fn fail(&mut self, reason: FailureReason, message: String) -> Vec<EventBody> {
        self.finished = true;
        vec![EventBody::RunFailed(RunFailed { reason, message })]
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
        vec![
            EventBody::ModelResponse(ModelResponse {
                text: std::mem::take(&mut self.text),
                finish_reason: self.stop_reason.take(),
            }),
            EventBody::UsageReported(UsageReported {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cost_micro_usd: cost,
            }),
            EventBody::RunCompleted(RunCompleted { summary: None }),
        ]
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
                let tool = block
                    .and_then(|b| b.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(block_type);
                self.fail(
                    FailureReason::ToolsNotEnabled,
                    format!(
                        "The model asked to use a tool ({}), but this agent has no tools yet. Nothing ran.",
                        clean(tool)
                    ),
                )
            }
            "content_block_delta" => {
                let delta = data.get("delta");
                let delta_type = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
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
            // ping, content_block_stop, and event kinds added later.
            _ => Vec::new(),
        }
    }
}
