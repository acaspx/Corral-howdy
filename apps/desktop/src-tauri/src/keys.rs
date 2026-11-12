//! The provider key commands (SK-3, docs/01 §11, security model T6).
//!
//! The webview sends a key in and only ever gets a [`KeyStatus`] back: whether a key is set and
//! its last 4 characters (TP-SETUP-01, TP-SEC-02). The key goes straight to the Keychain. Errors
//! never quote it. Each command needs its own grant in `capabilities/default.json`.

use corral_secrets::{KeyStatus, Provider, Secret, SecretStore, status};
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

/// Command: store a key typed into Settings.
#[tauri::command(async)]
pub fn save_provider_key(
    keys: State<'_, Keys>,
    provider: Provider,
    key: String,
) -> Result<KeyStatus, String> {
    save_key(keys.0.as_ref(), provider, key)
}

/// Command: remove a provider's key.
#[tauri::command(async)]
pub fn remove_provider_key(keys: State<'_, Keys>, provider: Provider) -> Result<KeyStatus, String> {
    remove_key(keys.0.as_ref(), provider)
}
