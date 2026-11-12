//! Provider API keys (SK-3, docs/01 §11, security model T6).
//!
//! - **Keys live in the macOS Keychain**, one item per provider (`corral.xai`, …), and nowhere
//!   else: never SQLite, logs, traces, manifests or sync.
//! - **Read at call time.** [`resolve`] fetches the key for each request, so a key changed in
//!   Settings applies to the next request, and a removed one stops the next request with
//!   "Add your xAI key" (TP-SKEL-01).
//! - **Never returned whole.** The UI gets a [`KeyStatus`]: whether a key is set, and its last 4
//!   characters (TP-SEC-02).
//! - **Hard to leak by accident.** [`Secret`] has no `Display`, `Serialize` or `Clone`; its `Debug`
//!   prints nothing of the key; its bytes are wiped when it's dropped; validation errors never
//!   quote it (TP-SEC-03).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
mod keychain;
#[cfg(target_os = "macos")]
pub use keychain::KeychainStore;

/// A model provider with its own API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    /// xAI (Grok).
    #[serde(rename = "xai")]
    Xai,
    /// OpenAI.
    #[serde(rename = "openai")]
    OpenAi,
    /// Anthropic.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// The Meta Model API.
    #[serde(rename = "meta")]
    Meta,
}

impl Provider {
    /// Every provider, in the order Settings lists them.
    pub const ALL: [Provider; 4] = [Self::Xai, Self::OpenAi, Self::Anthropic, Self::Meta];

    /// The stable ID used on the wire and in Keychain item names.
    pub fn id(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Meta => "meta",
        }
    }

    /// The name people know it by.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Xai => "xAI",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Meta => "Meta",
        }
    }
}

/// Why a key was refused or couldn't be stored. Never contains the key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// Nothing was entered.
    #[error("the key is empty")]
    Empty,
    /// Shorter than [`Secret::MIN_LEN`].
    #[error("that's too short to be an API key")]
    TooShort,
    /// Longer than [`Secret::MAX_LEN`].
    #[error("that's too long to be an API key")]
    TooLong,
    /// Spaces, line breaks or characters API keys don't use.
    #[error("API keys don't contain spaces, line breaks or special characters")]
    InvalidCharacters,
    /// The Keychain refused, with its status code.
    #[error("the Keychain refused (status {0})")]
    Keychain(i32),
    /// A stored item isn't valid text.
    #[error("the stored key is unreadable; remove it and add it again")]
    Unreadable,
    /// Internal state was poisoned by a panic.
    #[error("the key store's state is poisoned")]
    Poisoned,
}

/// An API key. See the crate docs for what it refuses to do.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Shortest key accepted.
    pub const MIN_LEN: usize = 8;
    /// Longest key accepted.
    pub const MAX_LEN: usize = 512;

    /// Validates a key as typed or pasted: surrounding whitespace is trimmed; the rest must be
    /// 8 to 512 printable ASCII characters with no spaces. `raw` is wiped either way.
    pub fn new(raw: impl Into<String>) -> Result<Self, SecretError> {
        let raw = Zeroizing::new(raw.into());
        let key = raw.trim();
        if key.is_empty() {
            return Err(SecretError::Empty);
        }
        if !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(SecretError::InvalidCharacters);
        }
        if key.len() < Self::MIN_LEN {
            return Err(SecretError::TooShort);
        }
        if key.len() > Self::MAX_LEN {
            return Err(SecretError::TooLong);
        }
        Ok(Self(Zeroizing::new(key.to_owned())))
    }

    /// The key, for the one place that sends it: the provider request.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The last 4 characters, the most Settings ever shows.
    pub fn last4(&self) -> String {
        let key = self.0.as_str();
        key[key.len().saturating_sub(4)..].to_owned()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

/// Where keys are kept: the Keychain in the app, memory in tests.
pub trait SecretStore: Send + Sync {
    /// The provider's key, if one is set.
    fn get(&self, provider: Provider) -> Result<Option<Secret>, SecretError>;
    /// Sets or replaces the provider's key.
    fn set(&self, provider: Provider, secret: &Secret) -> Result<(), SecretError>;
    /// Removes the provider's key. Removing a key that isn't there is fine.
    fn delete(&self, provider: Provider) -> Result<(), SecretError>;
}

/// The provider a key most likely belongs to, from its prefix (TP-SETUP-05): `xai-` is xAI,
/// `sk-ant-` is Anthropic, `sk-proj-`, `sk-svcacct-` and older `sk-` keys are OpenAI. Anything
/// else is `None`, so the user is asked rather than guessed for. A suggestion only: the provider's
/// own check decides whether the key works.
pub fn identify(key: &str) -> Option<Provider> {
    let key = key.trim();
    let rest_after = |prefix: &str| key.strip_prefix(prefix).filter(|rest| !rest.is_empty());
    if rest_after("xai-").is_some() {
        Some(Provider::Xai)
    } else if key.starts_with("sk-ant-") {
        rest_after("sk-ant-").map(|_| Provider::Anthropic)
    } else if rest_after("sk-").is_some() {
        Some(Provider::OpenAi)
    } else {
        None
    }
}

/// Whether a provider has a key, as Settings shows it (TP-SEC-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    /// The provider.
    pub provider: Provider,
    /// True when a key is stored.
    pub configured: bool,
    /// Its last 4 characters, when there is one.
    pub last4: Option<String>,
}

/// The provider's [`KeyStatus`].
pub fn status(store: &dyn SecretStore, provider: Provider) -> Result<KeyStatus, SecretError> {
    let secret = store.get(provider)?;
    Ok(KeyStatus {
        provider,
        configured: secret.is_some(),
        last4: secret.map(|s| s.last4()),
    })
}

/// Why a request has no key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// No key is stored. The message is what the UI shows.
    #[error("Add your {} key", .0.display_name())]
    Missing(Provider),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] SecretError),
}

/// The provider's key, read now. Call it for every request; never cache the result.
pub fn resolve(store: &dyn SecretStore, provider: Provider) -> Result<Secret, KeyError> {
    store.get(provider)?.ok_or(KeyError::Missing(provider))
}

/// Keys in memory, for tests and non-macOS builds. Wiped when dropped.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<Provider, Zeroizing<String>>>);

impl SecretStore for MemoryStore {
    fn get(&self, provider: Provider) -> Result<Option<Secret>, SecretError> {
        let keys = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        Ok(keys
            .get(&provider)
            .map(|k| Secret(Zeroizing::new(k.as_str().to_owned()))))
    }

    fn set(&self, provider: Provider, secret: &Secret) -> Result<(), SecretError> {
        let mut keys = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        keys.insert(provider, Zeroizing::new(secret.expose().to_owned()));
        Ok(())
    }

    fn delete(&self, provider: Provider) -> Result<(), SecretError> {
        let mut keys = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        keys.remove(&provider);
        Ok(())
    }
}
