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

/// How the key travels. Either way it's read by the caller at request time and used only by the
/// transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyHeader {
    /// `Authorization: Bearer <key>` (xAI, OpenAI).
    Bearer,
    /// The key alone in a named header, e.g. Anthropic's `x-api-key`.
    Named(&'static str),
}

impl KeyHeader {
    /// The header's name, lowercase.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bearer => "authorization",
            Self::Named(name) => name,
        }
    }
}

/// One POST. `headers` are fixed, non-secret headers (an API version); the key goes only in
/// [`KeyHeader`].
#[derive(Debug, Clone, Copy)]
pub struct PostRequest<'a> {
    /// Where to send it.
    pub url: &'a str,
    /// The key.
    pub key: &'a Secret,
    /// Which header carries the key.
    pub key_header: KeyHeader,
    /// Other headers, `(name, value)`.
    pub headers: &'a [(&'static str, &'static str)],
    /// The JSON body.
    pub body: &'a serde_json::Value,
}

/// Sends one request.
pub trait Transport: Send + Sync + 'static {
    /// POSTs `request.body` as JSON, asking for an event stream.
    fn post<'a>(
        &'a self,
        request: PostRequest<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>>;

    /// POSTs `body` as JSON to `url`, authorized with `bearer`, asking for an event stream.
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a Secret,
        body: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        self.post(PostRequest {
            url,
            key: bearer,
            key_header: KeyHeader::Bearer,
            headers: &[],
            body,
        })
    }
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
    fn post<'a>(
        &'a self,
        request: PostRequest<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let mut builder = self
                .client
                .post(request.url)
                .header(reqwest::header::ACCEPT, "text/event-stream");
            for (name, value) in request.headers {
                builder = builder.header(*name, *value);
            }
            builder = match request.key_header {
                // reqwest marks this header sensitive, so it's never printed in debug output.
                KeyHeader::Bearer => builder.bearer_auth(request.key.expose()),
                KeyHeader::Named(name) => {
                    let mut value = reqwest::header::HeaderValue::from_str(request.key.expose())
                        // The error would describe the key; it isn't passed on.
                        .map_err(|_| {
                            TransportError::Connect(
                                "the key has characters that can't go in a header".into(),
                            )
                        })?;
                    value.set_sensitive(true);
                    builder.header(name, value)
                }
            };
            let response = builder
                .json(request.body)
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

/// A form-encoded body: OAuth's token and revocation requests (ADR-017). Its values can be codes,
/// verifiers and tokens, so they're wiped when it's dropped and never printed.
#[derive(Default)]
pub struct Form(Vec<(&'static str, zeroize::Zeroizing<String>)>);

impl Form {
    /// An empty form.
    pub fn new() -> Self {
        Self::default()
    }

    /// The form with `name=value` added.
    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.0.push((name, zeroize::Zeroizing::new(value.into())));
        self
    }

    /// The first value for `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every `(name, value)`, in order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// `application/x-www-form-urlencoded`. Wiped when dropped.
    pub fn encode(&self) -> zeroize::Zeroizing<String> {
        let mut out = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in self.fields() {
            out.append_pair(name, value);
        }
        zeroize::Zeroizing::new(out.finish())
    }
}

impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Form(redacted)")
    }
}

/// The biggest OAuth reply read: far past a token response or a key set.
pub const MAX_FORM_REPLY: usize = 1024 * 1024;

/// Plain requests to an authorization server: form POSTs and GETs, answered with a status and a
/// whole (bounded) body. Separate from [`Transport`], which streams model replies.
pub trait FormTransport: Send + Sync + 'static {
    /// POSTs `form`, form-encoded, to `url`.
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>>;

    /// GETs `url`.
    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>>;

    /// GETs `url` with `bearer` as the `Authorization` header. A transport that can't refuses,
    /// so nothing is sent without the token by mistake.
    fn get_authorized<'a>(
        &'a self,
        url: &'a str,
        _bearer: &'a Secret,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            Err(TransportError::Exhausted(format!(
                "this transport can't send an authorized GET to {url}"
            )))
        })
    }
}

impl ReqwestTransport {
    async fn whole(response: reqwest::Response) -> Result<(u16, Vec<u8>), TransportError> {
        let status = response.status().as_u16();
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| TransportError::Body(e.without_url().to_string()))?;
            if body.len() + chunk.len() > MAX_FORM_REPLY {
                return Err(TransportError::Body("the reply is too large".into()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }
}

impl FormTransport for ReqwestTransport {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            let response = self
                .client
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header(reqwest::header::ACCEPT, "application/json")
                .body(form.encode().as_str().to_owned())
                .send()
                .await
                .map_err(|e| TransportError::Connect(e.without_url().to_string()))?;
            Self::whole(response).await
        })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| TransportError::Connect(e.without_url().to_string()))?;
            Self::whole(response).await
        })
    }
    fn get_authorized<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a Secret,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, "application/json")
                // reqwest marks this header sensitive, as for model requests.
                .bearer_auth(bearer.expose())
                .send()
                .await
                .map_err(|e| TransportError::Connect(e.without_url().to_string()))?;
            Self::whole(response).await
        })
    }
}
