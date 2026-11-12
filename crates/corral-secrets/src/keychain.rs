//! The macOS Keychain store: one generic-password item per provider, service `<prefix>.<provider>`
//! (`corral.xai` in the app), account `api-key`.
//!
//! Items are created by the signed app, so macOS limits access to it. During development every
//! rebuild changes an ad-hoc signature, and macOS asks once to allow the new build (ADR-001,
//! "Signing and identity").

use crate::{Provider, Secret, SecretError, SecretStore};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use zeroize::Zeroizing;

/// `errSecItemNotFound`: no such item.
const ITEM_NOT_FOUND: i32 = -25300;
const ACCOUNT: &str = "api-key";

/// Keys in the user's login Keychain.
pub struct KeychainStore {
    prefix: String,
}

impl KeychainStore {
    /// The app's store: items named `corral.<provider>`.
    pub fn corral() -> Self {
        Self::with_prefix("corral")
    }

    /// A store with another prefix, so tests never touch the app's items.
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    fn service(&self, provider: Provider) -> String {
        format!("{}.{}", self.prefix, provider.id())
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, provider: Provider) -> Result<Option<Secret>, SecretError> {
        match get_generic_password(&self.service(provider), ACCOUNT) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let text = std::str::from_utf8(&bytes).map_err(|_| SecretError::Unreadable)?;
                // A stored key went through `Secret::new` when it was saved; check it again so a
                // hand-edited item can't smuggle in anything else.
                Secret::new(text)
                    .map(Some)
                    .map_err(|_| SecretError::Unreadable)
            }
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(SecretError::Keychain(e.code())),
        }
    }

    fn set(&self, provider: Provider, secret: &Secret) -> Result<(), SecretError> {
        set_generic_password(&self.service(provider), ACCOUNT, secret.expose().as_bytes())
            .map_err(|e| SecretError::Keychain(e.code()))
    }

    fn delete(&self, provider: Provider) -> Result<(), SecretError> {
        match delete_generic_password(&self.service(provider), ACCOUNT) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(SecretError::Keychain(e.code())),
        }
    }
}
