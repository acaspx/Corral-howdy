//! Recorded exchanges, replayed in tests so they never call the network (AGENTS.md rule 7).
//!
//! A cassette file (`tests/cassettes/<provider>/*.json`) lists the response to each request in
//! order. Request headers are never stored, so a key can't end up in one; [`CassetteTransport`]
//! keeps what was sent in memory ([`SeenRequest`]) for tests to check.

use crate::transport::{BoxFuture, HttpResponse, PostRequest, Transport, TransportError};
use bytes::Bytes;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// A recorded conversation with a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cassette {
    /// What it exercises.
    pub description: String,
    /// The response to each request, in order.
    pub responses: Vec<RecordedResponse>,
}

/// One recorded response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedResponse {
    /// HTTP status.
    pub status: u16,
    /// `Retry-After`, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// Keep the stream open forever after the body (to test Stop).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stall_after_body: bool,
    /// The body, line by line.
    pub body: Vec<String>,
}

impl RecordedResponse {
    /// The body as the server sent it: the lines joined with `\n`, ending with one.
    pub fn body_bytes(&self) -> Vec<u8> {
        let mut text = self.body.join("\n");
        text.push('\n');
        text.into_bytes()
    }
}

/// Why a cassette couldn't be loaded.
#[derive(Debug, thiserror::Error)]
pub enum CassetteError {
    /// The file couldn't be read.
    #[error("couldn't read the cassette: {0}")]
    Io(#[from] std::io::Error),
    /// The file isn't a cassette.
    #[error("not a cassette: {0}")]
    Format(#[from] serde_json::Error),
}

impl Cassette {
    /// Loads a cassette file.
    pub fn load(path: &Path) -> Result<Self, CassetteError> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
}

/// What a test transport was asked to send. The key itself is never kept: only its last 4
/// characters, to check which key was used.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenRequest {
    /// Where it was sent.
    pub url: String,
    /// The key's last 4 characters.
    pub bearer_last4: String,
    /// The header that carried the key (`authorization` or e.g. `x-api-key`), lowercase.
    pub key_header: String,
    /// The other headers sent, `(name, value)`. Never the key.
    pub headers: Vec<(String, String)>,
    /// The JSON body.
    pub body: serde_json::Value,
}

/// A [`Transport`] that replays a cassette, splitting each body into `chunk`-byte pieces so the
/// stream parser sees arbitrary network boundaries.
pub struct CassetteTransport {
    responses: Mutex<VecDeque<RecordedResponse>>,
    chunk: usize,
    seen: Mutex<Vec<SeenRequest>>,
}

impl CassetteTransport {
    /// Replays `cassette`, `chunk` bytes at a time (at least 1).
    pub fn new(cassette: Cassette, chunk: usize) -> Self {
        Self {
            responses: Mutex::new(cassette.responses.into()),
            chunk: chunk.max(1),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every request sent so far, in order.
    pub fn seen(&self) -> Vec<SeenRequest> {
        // A poisoned lock still holds every request recorded before the panic.
        match self.seen.lock() {
            Ok(seen) => seen.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Transport for CassetteTransport {
    fn post<'a>(
        &'a self,
        request: PostRequest<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse, TransportError>> {
        Box::pin(async move {
            let poisoned = || TransportError::Exhausted("the cassette's state is poisoned".into());
            self.seen.lock().map_err(|_| poisoned())?.push(SeenRequest {
                url: request.url.to_owned(),
                bearer_last4: request.key.last4(),
                key_header: request.key_header.name().to_owned(),
                headers: request
                    .headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                body: request.body.clone(),
            });
            let next = self.responses.lock().map_err(|_| poisoned())?.pop_front();
            let response = next.ok_or_else(|| {
                TransportError::Exhausted("the cassette has no more responses".into())
            })?;
            let chunks: Vec<Result<Bytes, TransportError>> = response
                .body_bytes()
                .chunks(self.chunk)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect();
            let body = stream::iter(chunks);
            let body: crate::transport::ByteStream = if response.stall_after_body {
                Box::pin(futures_util::StreamExt::chain(body, stream::pending()))
            } else {
                Box::pin(body)
            };
            Ok(HttpResponse {
                status: response.status,
                retry_after: response.retry_after_secs.map(Duration::from_secs),
                body,
            })
        })
    }
}
