//! What every streamed model protocol shares: read the key at request time, ask the budget
//! meter, POST, retry rate limits, read the event stream with an idle timeout, and stop at once
//! on Stop. A [`Protocol`] says where to send, what to send and how to read the reply; xAI's
//! Responses ([`crate::responses`]) and Anthropic's Messages ([`crate::messages`]) are the two.
//!
//! - **Retries:** 429, 503 and 529 (Anthropic's "overloaded") are retried up to
//!   [`Protocol::max_attempts`] times in all, waiting for `Retry-After` when given, else 1 s,
//!   2 s, 4 s… (at most 60 s). Retries reuse the run's one admission: a refused attempt isn't
//!   billed.
//! - **A rejected key** (401, 403) isn't retried, and the provider's message isn't passed on: some
//!   repeat part of the key.
//! - **Keys come from a [`KeySource`]**, asked once per request: the stored API keys
//!   ([`StreamDriver::new`]) or a sign-in that renews its token at call time, such as a ChatGPT
//!   plan ([`crate::chatgpt::ChatGptPlan`], ADR-017).
//! - **A protocol can read its own errors** ([`Protocol::classify_error`]): the ChatGPT plan's
//!   route says by code whether to retry, and a usage limit stops at once.

use crate::sse::{SseEvent, SseParser};
use crate::transport::{BoxFuture, KeyHeader, PostRequest, Transport};
use corral_core::budget::{BudgetError, RequestEstimate};
use corral_core::event::{EventBody, FailureReason, ModelRequest, RunFailed};
use corral_core::supervisor::{
    CancelToken, EventSink, RunDriver, RunFuture, RunRequest, SupervisorError,
};
use corral_secrets::{KeyError, Provider, Secret, SecretStore, resolve};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// One provider's wire format.
pub trait Protocol: Send + Sync + 'static {
    /// Reads its event stream.
    type Decoder: StreamDecoder + Send;

    /// Whose key to use, and the name in messages.
    fn provider(&self) -> Provider;
    /// The model ID, for `model.request`.
    fn model(&self) -> &str;
    /// The most this request could cost, for the budget meter.
    fn estimate(&self, prompt: &str) -> RequestEstimate;
    /// The endpoint.
    fn url(&self) -> String;
    /// The request body, with the output limit the budget meter allowed.
    fn body(&self, prompt: &str, max_output_tokens: u32) -> Value;
    /// Which header carries the key.
    fn key_header(&self) -> KeyHeader;
    /// Fixed, non-secret headers (an API version).
    fn headers(&self) -> &'static [(&'static str, &'static str)];
    /// Attempts in all for a rate-limited or unavailable request.
    fn max_attempts(&self) -> u32;
    /// Fail the run if the stream sends nothing for this long.
    fn idle_timeout(&self) -> Duration;
    /// A fresh decoder for one response.
    fn decoder(&self) -> Self::Decoder;
    /// What to do with an error reply, read by the protocol itself; `None` leaves it to the
    /// shared rules above. Only consulted when [`Self::reads_errors`] is true.
    fn classify_error(&self, _status: u16, _body: &[u8]) -> Option<ErrorAction> {
        None
    }
    /// True when error replies go to [`Self::classify_error`] before the shared rules.
    fn reads_errors(&self) -> bool {
        false
    }
}

/// A protocol's verdict on an error reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorAction {
    /// Retry it, within [`Protocol::max_attempts`].
    Retry,
    /// End the run with this message (a provider failure).
    Stop(String),
}

/// Where a request's key comes from, asked once per request.
pub trait KeySource: Send + Sync + 'static {
    /// The key or token for `provider`, now.
    fn key(&self, provider: Provider) -> BoxFuture<'_, Result<Secret, KeyFailure>>;
}

/// Why a request has no key. Each message is what the card shows; none contains a key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyFailure {
    /// Nothing to send with: add a key, or sign in again.
    #[error("{0}")]
    Missing(String),
    /// The key can't be had right now (renewing it failed on the network); try later.
    #[error("{0}")]
    Unavailable(String),
    /// The store failed.
    #[error("{0}")]
    Internal(String),
}

/// The stored API keys, read at call time (TP-SKEL-01).
struct StoredKeys(Arc<dyn SecretStore>);

impl KeySource for StoredKeys {
    fn key(&self, provider: Provider) -> BoxFuture<'_, Result<Secret, KeyFailure>> {
        let result = match resolve(self.0.as_ref(), provider) {
            Ok(key) => Ok(key),
            Err(missing @ KeyError::Missing(_)) => Err(KeyFailure::Missing(missing.to_string())),
            Err(KeyError::Store(e)) => Err(KeyFailure::Internal(format!(
                "couldn't read the {} key: {e}",
                provider.display_name()
            ))),
        };
        Box::pin(async move { result })
    }
}

/// Turns a provider's stream events into Corral events.
pub trait StreamDecoder {
    /// The Corral events for one stream event.
    fn on_event(&mut self, event: &SseEvent) -> Vec<EventBody>;
    /// True once a terminal event (completed or failed) has been produced.
    fn is_finished(&self) -> bool;
}

/// A [`RunDriver`] that runs a prompt through a [`Protocol`].
pub struct StreamDriver<P> {
    transport: Arc<dyn Transport>,
    keys: Arc<dyn KeySource>,
    config: Arc<P>,
}

impl<P> Clone for StreamDriver<P> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            keys: self.keys.clone(),
            config: self.config.clone(),
        }
    }
}

impl<P: Protocol> StreamDriver<P> {
    /// A driver that sends through `transport`, reading keys from `keys`.
    pub fn new(transport: Arc<dyn Transport>, keys: Arc<dyn SecretStore>, config: P) -> Self {
        Self::with_keys(transport, Arc::new(StoredKeys(keys)), config)
    }

    /// A driver that sends through `transport`, asking `keys` for each request's key.
    pub fn with_keys(transport: Arc<dyn Transport>, keys: Arc<dyn KeySource>, config: P) -> Self {
        Self {
            transport,
            keys,
            config: Arc::new(config),
        }
    }
}

impl<P: Protocol> RunDriver for StreamDriver<P> {
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

/// Why a turn ended without finishing.
pub(crate) enum Failure {
    /// Not logged yet: the driver emits `run.failed`.
    Emit(FailureReason, String),
    /// The core already ended the run (the budget meter refused the request).
    Ended,
    /// The run is at its soft limit: nothing was sent. An agent that can ask the user does; one
    /// that can't ends the run.
    SoftLimit(String),
}

/// Logs `run.failed`.
pub(crate) fn fail_run(sink: &EventSink, reason: FailureReason, message: String) {
    // The sink refuses only when the run is already over or the log is gone; either way the
    // supervisor closes the run out.
    let _ = sink.emit(EventBody::RunFailed(RunFailed { reason, message }));
}

impl<P: Protocol> StreamDriver<P> {
    /// The protocol's configuration.
    pub(crate) fn config(&self) -> &P {
        self.config.as_ref()
    }

    async fn drive(&self, prompt: &str, sink: &EventSink) {
        let config = self.config.as_ref();
        let mut decoder = config.decoder();
        let result = self
            .turn(
                sink,
                &config.estimate(prompt),
                |max| config.body(prompt, max),
                &mut decoder,
            )
            .await;
        match result {
            Ok(()) | Err(Failure::Ended) => {}
            Err(Failure::Emit(reason, message)) => fail_run(sink, reason, message),
            // This driver's agents can't ask the user, so a soft limit ends the run here.
            Err(Failure::SoftLimit(message)) => {
                fail_run(sink, FailureReason::BudgetHardCap, message);
            }
        }
    }

    /// One request: reads the key, asks the budget meter with `estimate`, sends `body` (given
    /// the output limit the meter allowed), and feeds the stream to `decoder` until it finishes.
    pub(crate) async fn turn<D: StreamDecoder>(
        &self,
        sink: &EventSink,
        estimate: &RequestEstimate,
        body: impl FnOnce(u32) -> Value,
        decoder: &mut D,
    ) -> Result<(), Failure> {
        let config = self.config.as_ref();
        let provider = config.provider();
        let name = provider.display_name();
        let key = match self.keys.key(provider).await {
            Ok(key) => key,
            Err(KeyFailure::Missing(message) | KeyFailure::Unavailable(message)) => {
                return Err(Failure::Emit(FailureReason::Provider, message));
            }
            Err(KeyFailure::Internal(message)) => {
                return Err(Failure::Emit(FailureReason::Internal, message));
            }
        };
        let admission = sink
            .request_model(
                ModelRequest {
                    provider: provider.id().into(),
                    model: config.model().to_owned(),
                },
                estimate,
            )
            .map_err(|e| match e {
                SupervisorError::Budget(refusal @ BudgetError::SoftLimit { .. }) => {
                    Failure::SoftLimit(refusal.to_string())
                }
                // The core logged run.failed; nothing is sent.
                SupervisorError::Budget(
                    BudgetError::OverCap { .. }
                    | BudgetError::OverMonth { .. }
                    | BudgetError::NotAdmitted,
                ) => Failure::Ended,
                other @ (SupervisorError::Lifecycle(_)
                | SupervisorError::Log(_)
                | SupervisorError::DuplicateRun(_)
                | SupervisorError::UnknownRun(_)
                | SupervisorError::NotRunning(_)
                | SupervisorError::Poisoned) => Failure::Emit(
                    FailureReason::Internal,
                    format!("couldn't log the request: {other}"),
                ),
            })?;

        let url = config.url();
        let body = body(admission.max_output_tokens);
        let request = PostRequest {
            url: &url,
            key: &key,
            key_header: config.key_header(),
            headers: config.headers(),
            body: &body,
        };
        let mut attempt = 0;
        let response = loop {
            attempt += 1;
            let response = self.transport.post(request).await.map_err(|e| {
                Failure::Emit(
                    FailureReason::Provider,
                    format!("couldn't reach {name}: {e}"),
                )
            })?;
            if config.reads_errors() && !(200..=299).contains(&response.status) {
                let status = response.status;
                let retry_after = response.retry_after;
                let body = error_body(response.body).await;
                match config.classify_error(status, &body) {
                    Some(ErrorAction::Stop(message)) => {
                        return Err(Failure::Emit(FailureReason::Provider, message));
                    }
                    Some(ErrorAction::Retry) if attempt < config.max_attempts() => {
                        let backoff = Duration::from_secs(1 << (attempt - 1));
                        let wait = retry_after.unwrap_or(backoff).min(Duration::from_secs(60));
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    Some(ErrorAction::Retry) => {
                        return Err(Failure::Emit(
                            FailureReason::Provider,
                            format!(
                                "{name} was unavailable {attempt} times in a row. Try again in a minute."
                            ),
                        ));
                    }
                    None => {
                        return Err(Failure::Emit(
                            FailureReason::Provider,
                            format!("{name} returned HTTP {status}"),
                        ));
                    }
                }
            }
            match response.status {
                200..=299 => break response,
                401 | 403 => {
                    // Some providers' messages repeat part of the key; it isn't passed on.
                    return Err(Failure::Emit(
                        FailureReason::Provider,
                        format!(
                            "{name} rejected the key (HTTP {}). Check it in Settings.",
                            response.status
                        ),
                    ));
                }
                429 | 503 | 529 if attempt < config.max_attempts() => {
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
        let mut body = response.body;
        loop {
            let next = tokio::time::timeout(config.idle_timeout(), body.next())
                .await
                .map_err(|_| {
                    Failure::Emit(
                        FailureReason::Provider,
                        format!(
                            "{name} sent nothing for {} seconds",
                            config.idle_timeout().as_secs()
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
/// An error reply's first 16 KiB, for a protocol to read.
async fn error_body(mut body: crate::transport::ByteStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(Ok(chunk)) = body.next().await {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > 16 * 1024 {
            bytes.truncate(16 * 1024);
            break;
        }
    }
    bytes
}

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
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect::<String>()
        .trim()
        .to_owned()
}
