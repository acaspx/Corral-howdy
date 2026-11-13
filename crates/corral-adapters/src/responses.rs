//! The OpenAI Responses protocol (`POST /v1/responses`, streamed), as xAI serves it for Grok.
//! Walking skeleton (SK-4): text only, no tools.
//!
//! - **No tools** (TP-SKEL-02): the request carries no `tools`, `tool_choice` or provider-side
//!   tools, and `store: false` keeps the conversation off xAI's servers. If the model tries to call
//!   a tool anyway, the run fails with [`FailureReason::ToolsNotEnabled`] and nothing runs.
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
    /// xAI's API with the given model, priced at grok-4.7's list price ($2 per million input
    /// tokens, $6 per million output tokens, checked November 2026). Set [`Self::price`] for
    /// another model; the versioned price table replaces this in M2.
    pub fn xai(model: impl Into<String>) -> Self {
        Self {
            provider: Provider::Xai,
            base_url: "https://api.x.ai/v1".into(),
            model: model.into(),
            max_output_tokens: 16_384,
            price: Price::per_million_tokens(2_000_000, 6_000_000),
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
}

impl ResponseDecoder {
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
                    other => {
                        let tool = item
                            .and_then(|i| i.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or(other);
                        self.fail(
                            FailureReason::ToolsNotEnabled,
                            format!(
                                "The model asked to use a tool ({}), but this agent has no tools yet. Nothing ran.",
                                clean(tool)
                            ),
                        )
                    }
                }
            }
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
                let mut out = vec![EventBody::ModelResponse(ModelResponse {
                    text: std::mem::take(&mut self.text),
                    finish_reason,
                })];
                if let Some(usage) = response.and_then(|r| r.get("usage")) {
                    out.push(EventBody::UsageReported(usage_from(usage)));
                }
                out.push(EventBody::RunCompleted(RunCompleted { summary: None }));
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

    fn fail(&mut self, reason: FailureReason, message: String) -> Vec<EventBody> {
        self.finished = true;
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
