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
//!   429 and 503 are retried up to [`ResponsesConfig::max_attempts`] times in all, waiting for
//!   `Retry-After` when given.
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

use crate::sse::{SseEvent, SseParser};
use crate::transport::Transport;
use corral_core::budget::{Price, RequestEstimate, input_token_bound};
use corral_core::event::{
    EventBody, FailureReason, ModelDelta, ModelRequest, ModelResponse, RunCompleted, RunFailed,
    UsageReported,
};
use corral_core::supervisor::{
    CancelToken, EventSink, RunDriver, RunFuture, RunRequest, SupervisorError,
};
use corral_secrets::{KeyError, Provider, SecretStore, resolve};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
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
#[derive(Clone)]
pub struct ResponsesDriver {
    transport: Arc<dyn Transport>,
    keys: Arc<dyn SecretStore>,
    config: Arc<ResponsesConfig>,
}

impl ResponsesDriver {
    /// A driver that sends through `transport`, reading keys from `keys`.
    pub fn new(
        transport: Arc<dyn Transport>,
        keys: Arc<dyn SecretStore>,
        config: ResponsesConfig,
    ) -> Self {
        Self {
            transport,
            keys,
            config: Arc::new(config),
        }
    }
}

impl RunDriver for ResponsesDriver {
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let me = self.clone();
        Box::pin(async move {
            tokio::select! {
                biased;
                // Dropping `drive` drops the HTTP stream; the supervisor logs run.cancelled.
                () = cancel.cancelled() => {}
                () = me.drive(&request.prompt, &sink) => {}
            }
        })
    }
}

/// Why a run ended without completing.
enum Failure {
    /// Not logged yet: the driver emits `run.failed`.
    Emit(FailureReason, String),
    /// The core already ended the run (the budget meter refused the request).
    Ended,
}

impl ResponsesDriver {
    async fn drive(&self, prompt: &str, sink: &EventSink) {
        if let Err(Failure::Emit(reason, message)) = self.attempt(prompt, sink).await {
            // The sink refuses only when the run is already over or the log is gone; either way
            // the supervisor closes the run out.
            let _ = sink.emit(EventBody::RunFailed(RunFailed { reason, message }));
        }
    }

    async fn attempt(&self, prompt: &str, sink: &EventSink) -> Result<(), Failure> {
        let config = &self.config;
        let name = config.provider.display_name();
        let key = match resolve(self.keys.as_ref(), config.provider) {
            Ok(key) => key,
            Err(missing @ KeyError::Missing(_)) => {
                return Err(Failure::Emit(FailureReason::Provider, missing.to_string()));
            }
            Err(KeyError::Store(e)) => {
                return Err(Failure::Emit(
                    FailureReason::Internal,
                    format!("couldn't read the {name} key: {e}"),
                ));
            }
        };
        let admission = sink
            .request_model(
                ModelRequest {
                    provider: config.provider.id().into(),
                    model: config.model.clone(),
                },
                &config.estimate(prompt),
            )
            .map_err(|e| match e {
                // The core logged run.failed; nothing is sent.
                SupervisorError::Budget(_) => Failure::Ended,
                other @ (SupervisorError::Lifecycle(_)
                | SupervisorError::Log(_)
                | SupervisorError::DuplicateRun(_)
                | SupervisorError::UnknownRun(_)
                | SupervisorError::Poisoned) => Failure::Emit(
                    FailureReason::Internal,
                    format!("couldn't log the request: {other}"),
                ),
            })?;

        let url = config.url();
        let body = config.request_body_limited(prompt, admission.max_output_tokens);
        let mut attempt = 0;
        let response = loop {
            attempt += 1;
            let response = self
                .transport
                .post_json(&url, &key, &body)
                .await
                .map_err(|e| {
                    Failure::Emit(
                        FailureReason::Provider,
                        format!("couldn't reach {name}: {e}"),
                    )
                })?;
            match response.status {
                200..=299 => break response,
                401 | 403 => {
                    // xAI's message repeats part of the key; it isn't passed on.
                    return Err(Failure::Emit(
                        FailureReason::Provider,
                        format!(
                            "{name} rejected the key (HTTP {}). Check it in Settings.",
                            response.status
                        ),
                    ));
                }
                429 | 503 if attempt < config.max_attempts => {
                    let backoff = Duration::from_secs(1 << (attempt - 1));
                    let wait = response
                        .retry_after
                        .unwrap_or(backoff)
                        .min(Duration::from_secs(60));
                    tokio::time::sleep(wait).await;
                }
                429 => {
                    return Err(Failure::Emit(
                        FailureReason::Provider,
                        format!(
                            "{name}'s rate limit was hit {attempt} times in a row. Try again in a minute."
                        ),
                    ));
                }
                status => {
                    let detail = error_detail(response.body).await;
                    return Err(Failure::Emit(
                        FailureReason::Provider,
                        format!("{name} returned HTTP {status}{detail}"),
                    ));
                }
            }
        };

        let mut parser = SseParser::default();
        let mut decoder = ResponseDecoder::default();
        let mut body = response.body;
        loop {
            let next = tokio::time::timeout(config.idle_timeout, body.next())
                .await
                .map_err(|_| {
                    Failure::Emit(
                        FailureReason::Provider,
                        format!(
                            "{name} sent nothing for {} seconds",
                            config.idle_timeout.as_secs()
                        ),
                    )
                })?;
            let chunk = match next {
                None => break,
                Some(Ok(chunk)) => chunk,
                Some(Err(e)) => {
                    return Err(Failure::Emit(
                        FailureReason::Provider,
                        format!("{name}: {e}"),
                    ));
                }
            };
            let events = parser.push(&chunk).map_err(|e| {
                Failure::Emit(
                    FailureReason::Adapter,
                    format!("{name} sent an unreadable stream: {e}"),
                )
            })?;
            for event in &events {
                for out in decoder.on_event(event) {
                    emit(sink, out)?;
                }
                if decoder.is_finished() {
                    return Ok(());
                }
            }
        }
        Err(Failure::Emit(
            FailureReason::Adapter,
            format!("{name}'s stream ended before the response finished"),
        ))
    }
}

fn emit(sink: &EventSink, body: EventBody) -> Result<(), Failure> {
    sink.emit(body).map_err(|e| {
        Failure::Emit(
            FailureReason::Internal,
            format!("couldn't log an event: {e}"),
        )
    })
}

/// `: <message>` from an error body (`{"error": "..."}` or `{"error": {"message": "..."}}`),
/// cleaned and shortened; empty if there's nothing usable. Reads at most 16 KB.
async fn error_detail(mut body: crate::transport::ByteStream) -> String {
    let mut bytes = Vec::new();
    while let Some(Ok(chunk)) = body.next().await {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > 16 * 1024 {
            break;
        }
    }
    let message = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| {
        let e = v.get("error")?;
        e.as_str()
            .or_else(|| e.get("message").and_then(Value::as_str))
            .map(str::to_owned)
    });
    match message {
        Some(m) if !clean(&m).is_empty() => format!(": {}", clean(&m)),
        _ => String::new(),
    }
}

/// Provider text for a card or trace: no control characters, at most 300 characters.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect::<String>()
        .trim()
        .to_owned()
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
