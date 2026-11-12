//! The network: one HTTPS POST with a JSON body and a streamed response. Everything above this
//! module is testable without it ([`crate::cassette`]).

use bytes::Bytes;
use corral_secrets::Secret;
use futures_util::{Stream, StreamExt};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A response body, chunk by chunk.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send>>;

/// A boxed future, so [`Transport`] can be used as `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The status line and headers that matter, plus the body as it arrives.
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// `Retry-After`, in seconds, when the server sent one.
    pub retry_after: Option<Duration>,
    /// The body.
    pub body: ByteStream,
}

/// Why a request didn't get a response. Never contains the key or headers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// No connection, DNS, TLS or timeout before a response.
    #[error("couldn't connect: {0}")]
    Connect(String),
    /// The body broke off.
    #[error("the connection broke: {0}")]
    Body(String),
    /// A test transport ran out of recorded responses.
    #[error("{0}")]
    Exhausted(String),
}

/// Sends one request. The bearer key is read by the caller at request time and used only here.
pub trait Transport: Send + Sync + 'static {
    /// POSTs `body` as JSON to `url`, authorized with `bearer`, asking for an event stream.
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a Secret,
        body: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>>;
}

/// The real network, through `reqwest` with the system's TLS.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// A client with a 10-second connect timeout. No overall timeout: reasoning models can take
    /// minutes, and the driver applies its own idle timeout to the stream.
    pub fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| TransportError::Connect(e.without_url().to_string()))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a Secret,
        body: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let response = self
                .client
                .post(url)
                // reqwest marks this header sensitive, so it's never printed in debug output.
                .bearer_auth(bearer.expose())
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(body)
                .send()
                .await
                .map_err(|e| TransportError::Connect(e.without_url().to_string()))?;
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let status = response.status().as_u16();
            let body = response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| TransportError::Body(e.without_url().to_string())));
            Ok(HttpResponse {
                status,
                retry_after,
                body: Box::pin(body),
            })
        })
    }
}
