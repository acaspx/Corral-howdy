//! Checking a key with its provider before Corral stores it (TP-SETUP-02, docs/01 §11).
//!
//! The check asks the provider for its model list: it proves the key works and spends no tokens.
//! A refused key (401 or 403) is reported as such; anything else that goes wrong (the provider is
//! down, rate-limited or unreachable) says it couldn't check, so a good key isn't called bad. A
//! provider without a known check (the Meta Model API, for now) is stored unchecked, and the UI
//! says so. Messages never quote the key; the key only travels in the request's auth header.
//!
//! Before anything is sent, two common copying mistakes are caught (TP-SETUP-06): a key's ID
//! (`apikey_…`) and a key as a console's list shows it, with the middle left out. Either would only
//! come back as a refused key, which doesn't say what to copy instead.

use crate::transport::BoxFuture;
use corral_secrets::{Provider, Secret};
use std::time::Duration;

/// The result of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider accepted the key.
    Accepted,
    /// There's no check for this provider yet; the key is stored as given.
    NotChecked,
}

/// Why a key wasn't accepted. The message is what the UI shows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyCheckError {
    /// The provider refused the key.
    #[error("{} didn't accept this key.", .0.display_name())]
    Rejected(Provider),
    /// The provider couldn't be asked; the key may well be fine.
    #[error("Couldn't reach {} to check the key ({}). Try again in a moment.", .0.display_name(), .1)]
    Unavailable(Provider, String),
    /// What was pasted isn't a key, and nothing was sent.
    #[error("{0}")]
    NotTheKey(KeyMistake),
}

/// Something pasted in place of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyMistake {
    /// The key's ID, which consoles list beside the key.
    #[error(
        "That's the key's ID, not the key. Copy the key itself, which you see when you create it."
    )]
    KeyId,
    /// The key as a list shows it, with the middle left out.
    #[error(
        "That's a shortened copy of the key, with the middle left out. Copy the whole key, which you see when you create it."
    )]
    Shortened,
}

/// The copying mistake `key` shows, if any. Real keys have no `apikey_` prefix and no run of dots
/// or stars.
fn mistake(key: &str) -> Option<KeyMistake> {
    if key.starts_with("apikey_") {
        Some(KeyMistake::KeyId)
    } else if key.contains("...") || key.contains("**") {
        Some(KeyMistake::Shortened)
    } else {
        None
    }
}

/// One authenticated GET that reports only the HTTP status. The network lives in
/// [`ReqwestProbe`]; tests pass a fake.
pub trait ModelsProbe: Send + Sync {
    /// GETs `url` with `headers` and returns the status code, or why there was none. The error
    /// text must not contain header values.
    fn get<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>>;
}

/// Where and how each provider lists its models, or `None` when there's no known check.
pub(crate) fn models_request(
    provider: Provider,
    key: &Secret,
) -> Option<(&'static str, Vec<(&'static str, String)>)> {
    let bearer = || vec![("authorization", format!("Bearer {}", key.expose()))];
    match provider {
        Provider::Xai => Some(("https://api.x.ai/v1/models", bearer())),
        Provider::OpenAi => Some(("https://api.openai.com/v1/models", bearer())),
        Provider::Anthropic => Some((
            "https://api.anthropic.com/v1/models",
            vec![
                ("x-api-key", key.expose().to_owned()),
                ("anthropic-version", "2023-06-01".to_owned()),
            ],
        )),
        Provider::Meta => None,
    }
}

/// Checks `key` with `provider`.
pub async fn check_key(
    probe: &dyn ModelsProbe,
    provider: Provider,
    key: &Secret,
) -> Result<KeyCheck, KeyCheckError> {
    if let Some(mistake) = mistake(key.expose()) {
        return Err(KeyCheckError::NotTheKey(mistake));
    }
    let Some((url, headers)) = models_request(provider, key) else {
        return Ok(KeyCheck::NotChecked);
    };
    match probe.get(url, &headers).await {
        Ok(200..=299) => Ok(KeyCheck::Accepted),
        Ok(401 | 403) => Err(KeyCheckError::Rejected(provider)),
        Ok(status) => Err(KeyCheckError::Unavailable(
            provider,
            format!("it answered {status}"),
        )),
        Err(reason) => Err(KeyCheckError::Unavailable(provider, reason)),
    }
}

/// The real probe: HTTPS through the system's TLS, 15 seconds at most.
pub struct ReqwestProbe {
    client: reqwest::Client,
}

impl ReqwestProbe {
    /// A client with a 10-second connect timeout and a 15-second overall timeout.
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.without_url().to_string())?;
        Ok(Self { client })
    }
}

impl ModelsProbe for ReqwestProbe {
    fn get<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        Box::pin(async move {
            let mut request = self.client.get(url);
            for (name, value) in headers {
                let mut value = reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| "the key can't be sent as a header".to_owned())?;
                // Never printed in debug output.
                value.set_sensitive(true);
                request = request.header(*name, value);
            }
            let response = request.send().await.map_err(|e| {
                if e.is_timeout() {
                    "it didn't answer in time".to_owned()
                } else {
                    "no connection".to_owned()
                }
            })?;
            Ok(response.status().as_u16())
        })
    }
}
