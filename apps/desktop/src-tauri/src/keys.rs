//! The provider key commands (SK-3, docs/01 §11, security model T6).
//!
//! One key field takes any provider's key. The core suggests the provider from the key's prefix
//! (TP-SETUP-05), checks the key with that provider before storing it (TP-SETUP-02), and the
//! webview only ever gets a [`KeyStatus`] back: whether a key is set and its last 4 characters
//! (TP-SETUP-01, TP-SEC-02). The key goes straight to the Keychain. Errors never quote it. Each
//! command needs its own grant in `capabilities/default.json`.

use corral_adapters::transport::BoxFuture;
use corral_adapters::verify::{KeyCheck, ModelsProbe, ReqwestProbe, check_key};
use corral_secrets::{KeyStatus, Provider, Secret, SecretStore, identify, status};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// The key store the commands use.
pub struct Keys(pub Arc<dyn SecretStore>);

impl Keys {
    /// The login Keychain on macOS. Other platforms keep keys in memory for the session; Corral
    /// only ships on macOS, and the CI builds on Linux never store a real key.
    pub fn system() -> Self {
        #[cfg(target_os = "macos")]
        let store: Arc<dyn SecretStore> = Arc::new(corral_secrets::KeychainStore::corral());
        #[cfg(not(target_os = "macos"))]
        let store: Arc<dyn SecretStore> = Arc::new(corral_secrets::MemoryStore::default());
        Self(store)
    }
}

/// How keys are checked with their providers: HTTPS in the app, a fake in tests.
pub struct KeyChecker(pub Arc<dyn ModelsProbe>);

impl KeyChecker {
    /// HTTPS to the providers. If the client can't be built, every check says why and no key is
    /// stored unchecked.
    pub fn system() -> Self {
        match ReqwestProbe::new() {
            Ok(probe) => Self(Arc::new(probe)),
            Err(reason) => Self(Arc::new(NoClient(format!("no HTTPS client: {reason}")))),
        }
    }
}

/// A probe that can't reach anyone.
struct NoClient(String);

impl ModelsProbe for NoClient {
    fn get<'a>(
        &'a self,
        _url: &'a str,
        _headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        let reason = self.0.clone();
        Box::pin(async move { Err(reason) })
    }
}

/// A connected key: its status, and whether the provider checked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connected {
    /// The provider's key status.
    pub status: KeyStatus,
    /// Whether the provider accepted the key (`false`: there's no check for it yet).
    pub checked: bool,
}

/// The provider `key` most likely belongs to, for the field's hint.
pub fn identify_key(key: &str) -> Option<Provider> {
    identify(key)
}

/// Validates `key`, checks it with `provider`, and stores it only if the provider accepted it
/// (or has no check yet). `key` is wiped either way.
pub async fn connect_key(
    store: &dyn SecretStore,
    probe: &dyn ModelsProbe,
    provider: Provider,
    key: String,
) -> Result<Connected, String> {
    let secret = Secret::new(key).map_err(|e| e.to_string())?;
    let checked = match check_key(probe, provider, &secret)
        .await
        .map_err(|e| e.to_string())?
    {
        KeyCheck::Accepted => true,
        KeyCheck::NotChecked => false,
    };
    store.set(provider, &secret).map_err(|e| e.to_string())?;
    Ok(Connected {
        status: key_status(store, provider)?,
        checked,
    })
}

/// Whether `provider` has a key.
pub fn key_status(store: &dyn SecretStore, provider: Provider) -> Result<KeyStatus, String> {
    status(store, provider).map_err(|e| e.to_string())
}

/// Validates and stores `key`, then returns the provider's status. `key` is wiped either way.
pub fn save_key(
    store: &dyn SecretStore,
    provider: Provider,
    key: String,
) -> Result<KeyStatus, String> {
    let secret = Secret::new(key).map_err(|e| e.to_string())?;
    store.set(provider, &secret).map_err(|e| e.to_string())?;
    key_status(store, provider)
}

/// Removes `provider`'s key and returns its (now empty) status.
pub fn remove_key(store: &dyn SecretStore, provider: Provider) -> Result<KeyStatus, String> {
    store.delete(provider).map_err(|e| e.to_string())?;
    key_status(store, provider)
}

// Async so a Keychain access prompt never blocks the main thread.

/// Command: whether `provider` has a key.
#[tauri::command(async)]
pub fn provider_key_status(keys: State<'_, Keys>, provider: Provider) -> Result<KeyStatus, String> {
    key_status(keys.0.as_ref(), provider)
}

/// Command: which provider a key typed into the field looks like.
#[tauri::command]
pub fn identify_provider_key(key: String) -> Option<Provider> {
    identify_key(&key)
}

/// Command: check a key with its provider, then store it.
#[tauri::command]
pub async fn connect_provider_key(
    keys: State<'_, Keys>,
    checker: State<'_, KeyChecker>,
    provider: Provider,
    key: String,
) -> Result<Connected, String> {
    connect_key(keys.0.as_ref(), checker.0.as_ref(), provider, key).await
}

/// Command: remove a provider's key.
#[tauri::command(async)]
pub fn remove_provider_key(keys: State<'_, Keys>, provider: Provider) -> Result<KeyStatus, String> {
    remove_key(keys.0.as_ref(), provider)
}
