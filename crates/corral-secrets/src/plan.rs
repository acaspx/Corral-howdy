//! A ChatGPT plan's tokens (ADR-017, security model T31).
//!
//! Sign in with ChatGPT returns an ID token, an access token and a rotating refresh token for one
//! issued client. They are kept together as one [`PlanCredentials`] record, in the plan's own
//! Keychain item (`corral.openai-plan`, account `oauth`), never next to an API key and never
//! anywhere else. The record's `Debug` shows the account but none of the tokens, and every token
//! goes back through [`Secret::token`] when it's read, so a hand-edited item can't smuggle in
//! anything else.

use crate::{Secret, SecretError};
use serde::Deserialize;
use std::fmt;
use std::sync::Mutex;
use zeroize::Zeroizing;

/// The plan's Keychain item, after the store's prefix: `corral.openai-plan`.
pub const PLAN_ITEM: &str = "openai-plan";
/// The item's account.
pub const PLAN_ACCOUNT: &str = "oauth";

/// One signed-in ChatGPT account's credentials, as OpenAI's open-source flow says to keep them.
pub struct PlanCredentials {
    /// The account's email, from the verified ID token.
    pub email: String,
    /// The verified ID token's `sub`: the account's identity.
    pub subject: String,
    /// The client OpenAI issued at registration (never `dynamic_agent_client`).
    pub client_id: String,
    /// This Mac's `ext_agent_host_id`.
    pub host_id: String,
    /// The last ID token, sent back as `id_token_hint` when signing in again.
    pub id_token: Secret,
    /// The bearer token for plan requests.
    pub access_token: Secret,
    /// The rotating refresh token.
    pub refresh_token: Secret,
    /// When the access token expires, in Unix milliseconds.
    pub expires_at_ms: u64,
    /// The scopes OpenAI granted.
    pub scopes: Vec<String>,
    /// When these tokens were received, in Unix milliseconds.
    pub saved_at_ms: u64,
}

impl fmt::Debug for PlanCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanCredentials")
            .field("email", &self.email)
            .field("client_id", &self.client_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// The stored form, read back. Wiped when dropped.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    email: String,
    subject: String,
    client_id: String,
    host_id: String,
    id_token: String,
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    scopes: Vec<String>,
    saved_at_ms: u64,
}

impl Drop for Record {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.id_token.zeroize();
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

impl PlanCredentials {
    /// The record as JSON, for the store. Wiped when dropped.
    pub fn to_record(&self) -> Zeroizing<String> {
        // A JSON value's text can't fail to build, unlike a derived serializer's.
        let mut value = serde_json::json!({
            "email": self.email,
            "subject": self.subject,
            "client_id": self.client_id,
            "host_id": self.host_id,
            "id_token": self.id_token.expose(),
            "access_token": self.access_token.expose(),
            "refresh_token": self.refresh_token.expose(),
            "expires_at_ms": self.expires_at_ms,
            "scopes": self.scopes,
            "saved_at_ms": self.saved_at_ms,
        });
        let text = Zeroizing::new(value.to_string());
        wipe(&mut value);
        text
    }

    /// Reads a stored record. Anything that isn't one, or whose tokens aren't tokens, is
    /// [`SecretError::Unreadable`].
    pub fn from_record(json: &str) -> Result<Self, SecretError> {
        let record: Record = serde_json::from_str(json).map_err(|_| SecretError::Unreadable)?;
        let token = |raw: &str| Secret::token(raw).map_err(|_| SecretError::Unreadable);
        Ok(Self {
            id_token: token(&record.id_token)?,
            access_token: token(&record.access_token)?,
            refresh_token: token(&record.refresh_token)?,
            email: record.email.clone(),
            subject: record.subject.clone(),
            client_id: record.client_id.clone(),
            host_id: record.host_id.clone(),
            expires_at_ms: record.expires_at_ms,
            scopes: record.scopes.clone(),
            saved_at_ms: record.saved_at_ms,
        })
    }
}

/// Where the plan's credentials are kept: the Keychain in the app, memory in tests.
pub trait PlanStore: Send + Sync {
    /// The signed-in account's credentials, if any.
    fn get_plan(&self) -> Result<Option<PlanCredentials>, SecretError>;
    /// Sets or replaces them.
    fn set_plan(&self, credentials: &PlanCredentials) -> Result<(), SecretError>;
    /// Removes them. Removing what isn't there is fine.
    fn delete_plan(&self) -> Result<(), SecretError>;
}

/// The plan's credentials in memory, for tests and non-macOS builds. Wiped when dropped.
#[derive(Default)]
pub struct MemoryPlanStore(Mutex<Option<Zeroizing<String>>>);

impl PlanStore for MemoryPlanStore {
    fn get_plan(&self) -> Result<Option<PlanCredentials>, SecretError> {
        let slot = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        slot.as_deref()
            .map(|json| PlanCredentials::from_record(json))
            .transpose()
    }

    fn set_plan(&self, credentials: &PlanCredentials) -> Result<(), SecretError> {
        let mut slot = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        *slot = Some(credentials.to_record());
        Ok(())
    }

    fn delete_plan(&self) -> Result<(), SecretError> {
        let mut slot = self.0.lock().map_err(|_| SecretError::Poisoned)?;
        *slot = None;
        Ok(())
    }
}

/// Overwrites every string in `value`, so the tokens in it don't outlive the record.
fn wipe(value: &mut serde_json::Value) {
    use zeroize::Zeroize;
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(wipe),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(wipe),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}
