//! A ChatGPT plan, through Sign in with ChatGPT (ADR-017, TP-SUB-01..03).
//!
//! OpenAI's flow for open-source and locally run apps
//! (developers.openai.com/siwc/token-sharing-open-source, checked 2026-11-25):
//!
//! 1. **Register on first sign-in.** [`Attempt::begin`] builds the authorization URL with
//!    `client_id=dynamic_agent_client`, the app's name and this Mac's [`HostId`]; signing in again
//!    uses the client OpenAI issued, the last ID token and the email as hints. Every attempt has
//!    fresh `state`, `nonce` and PKCE (S256), and a loopback redirect on `127.0.0.1` whose port is
//!    the only thing that changes.
//! 2. **The callback must carry the attempt's state** ([`Attempt::callback`]), and a new
//!    registration its issued client; a returning one may not switch clients.
//! 3. **The code is exchanged** with the issued client and the verifier ([`Attempt::exchange_form`]).
//! 4. **The ID token is verified** against OpenAI's published keys (RS256 only), its issuer,
//!    audience, expiry and nonce, and **the plan scope must have been granted**
//!    (`chatgpt.tokens.use.direct`); without it nothing is kept ([`Attempt::finish`]).
//! 5. **The tokens go only to the plan's store** ([`PlanStore`], the Keychain item
//!    `corral.openai-plan`), are refreshed at call time one caller at a time, rotate, and are
//!    revoked at OpenAI on sign-out ([`ChatGptPlan`]).
//!
//! The plan's Responses route and its error codes are in [`crate::responses`] and
//! [`plan_failure`]. Nothing here logs or prints a code, verifier or token: they live in
//! [`Secret`] and [`Form`], whose `Debug` is redacted.

use crate::responses::ResponsesConfig;
use crate::stream::{KeyFailure, KeySource, fail_run};
use crate::transport::{BoxFuture, Form, FormTransport};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use corral_core::clock::Clock;
use corral_core::event::FailureReason;
use corral_core::ids::AgentId;
use corral_core::supervisor::{CancelToken, EventSink, RunDriver, RunFuture, RunRequest};
use corral_secrets::{PlanCredentials, PlanStore, Provider, Secret, SecretError};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{digest, signature};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zeroize::Zeroizing;

/// Where the browser signs in.
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
/// Code exchange and refresh.
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// Sign-out: the `revocation_endpoint` in OpenAI's OpenID configuration.
pub const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
/// OpenAI's published signing keys.
pub const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
/// The ID token's issuer.
pub const ISSUER: &str = "https://auth.openai.com";
/// The API the plan's tokens are for.
pub const RESOURCE: &str = "https://api.openai.com/v1";
/// The first-registration client.
pub const REGISTER_CLIENT: &str = "dynamic_agent_client";
/// The app's name, the same on every Mac.
pub const AGENT_NAME: &str = "Corral";
/// Identity and plan usage.
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope that lets Corral use the plan. A valid ID token without it is not enough.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// Where the user sees and limits the plan's usage.
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
/// The loopback callback's path.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// Refresh when the access token has less than this left.
const REFRESH_AHEAD_MS: u64 = 5 * 60 * 1000;
/// Clock skew allowed when checking an ID token's expiry.
const LEEWAY_SECS: u64 = 60;
/// Attempts to revoke on sign-out when OpenAI can't be reached.
const REVOKE_ATTEMPTS: u32 = 3;

/// Why signing in didn't finish. No variant carries a code, verifier or token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignInError {
    /// The system's random source failed.
    #[error("couldn't make a sign-in request (no randomness)")]
    Random,
    /// Not an `ext_agent_host_id` OpenAI accepts.
    #[error("this Mac's sign-in ID is unreadable")]
    HostId,
    /// The browser came back with someone else's request, or none.
    #[error("the sign-in reply didn't match the request; try again")]
    StateMismatch,
    /// The user said no.
    #[error("ChatGPT sign-in was cancelled")]
    Denied,
    /// OpenAI returned an error instead of a code.
    #[error("ChatGPT sign-in failed ({0})")]
    Failed(String),
    /// No code came back.
    #[error("the sign-in reply had no code; try again")]
    MissingCode,
    /// A first sign-in came back without the client OpenAI issued.
    #[error("the sign-in reply had no client ID; try again")]
    NoClientId,
    /// A returning sign-in came back for a different client.
    #[error("the sign-in reply was for a different registration; try again")]
    ClientMismatch,
    /// The token endpoint refused, with its error code (or the HTTP status).
    #[error("ChatGPT refused the sign-in ({0}); try again")]
    Exchange(String),
    /// The token endpoint's reply wasn't a token response.
    #[error("ChatGPT's sign-in reply was unreadable")]
    TokenResponse,
    /// OpenAI's signing keys couldn't be fetched or read.
    #[error("couldn't get ChatGPT's signing keys")]
    Keys,
    /// The ID token failed a check.
    #[error("ChatGPT's ID token didn't check out ({0:?})")]
    IdToken(IdTokenError),
    /// Signed in, but plan usage wasn't allowed.
    #[error(
        "ChatGPT plan usage wasn't allowed, so nothing was saved. Sign in again and allow it, or add an OpenAI key"
    )]
    PlanNotGranted,
    /// OpenAI couldn't be reached.
    #[error("couldn't reach ChatGPT: {0}")]
    Network(String),
    /// The Keychain refused.
    #[error(transparent)]
    Store(#[from] SecretError),
}

/// Which check an ID token failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdTokenError {
    /// Not three base64url parts with JSON in the first two.
    Malformed,
    /// Not RS256 (`none`, HMAC and the rest are refused).
    Algorithm,
    /// Signed by a key OpenAI doesn't publish.
    UnknownKey,
    /// The signature doesn't verify.
    Signature,
    /// Not issued by `https://auth.openai.com`.
    Issuer,
    /// Not for the issued client.
    Audience,
    /// Expired.
    Expired,
    /// Not for this attempt.
    Nonce,
    /// No subject or email.
    Claims,
}

/// Why a refresh didn't renew the tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshError {
    /// The refresh token is dead: clear the tokens and sign in again.
    SignInAgain,
    /// OpenAI refused Corral's registration (`invalid_client`).
    BadClient,
    /// OpenAI couldn't be reached or failed: keep the tokens and try later.
    Unavailable,
}

/// This Mac's `ext_agent_host_id`: opaque, made once and kept (it isn't a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostId(String);

impl HostId {
    /// A new `urn:uuid:` (version 4) ID, for a Mac that has none yet.
    pub fn generate() -> Result<Self, SignInError> {
        let mut b = [0u8; 16];
        SystemRandom::new()
            .fill(&mut b)
            .map_err(|_| SignInError::Random)?;
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        Ok(Self(format!(
            "urn:uuid:{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )))
    }

    /// A kept ID, in one of the forms OpenAI accepts: `urn:uuid:…`, a JWK thumbprint URI or
    /// `did:key:…`.
    pub fn parse(text: &str) -> Result<Self, SignInError> {
        let ok = if let Some(uuid) = text.strip_prefix("urn:uuid:") {
            uuid.len() == 36
                && uuid.char_indices().all(|(i, c)| match i {
                    8 | 13 | 18 | 23 => c == '-',
                    _ => c.is_ascii_hexdigit(),
                })
        } else if let Some(rest) = text.strip_prefix("urn:ietf:params:oauth:jwk-thumbprint:") {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'-' | b'_'))
        } else if let Some(rest) = text.strip_prefix("did:key:") {
            !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_alphanumeric())
        } else {
            false
        };
        if ok {
            Ok(Self(text.to_owned()))
        } else {
            Err(SignInError::HostId)
        }
    }

    /// The ID as sent.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A saved account signing in again.
pub struct Returning {
    /// Its issued client.
    pub client_id: String,
    /// Its last ID token, for `id_token_hint` (it may have expired).
    pub id_token: Option<Secret>,
    /// Its email, for `login_hint`.
    pub email: Option<String>,
}

/// The signed-in account, as the window shows it: no tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanAccount {
    /// The account's email.
    pub email: String,
    /// Its issued client, kept after sign-out so signing in again reuses it.
    pub client_id: String,
    /// The verified subject.
    pub subject: String,
}

impl PlanAccount {
    fn of(c: &PlanCredentials) -> Self {
        Self {
            email: c.email.clone(),
            client_id: c.client_id.clone(),
            subject: c.subject.clone(),
        }
    }
}

/// What signing out did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedOut {
    /// False when OpenAI couldn't confirm the revocation: the tokens are deleted here anyway,
    /// and the window says to disconnect Corral in ChatGPT's settings.
    pub revoked: bool,
}

/// One sign-in, from opening the browser to the stored tokens.
pub struct Attempt {
    host: String,
    port: u16,
    client_id: Option<String>,
    id_token_hint: Option<Zeroizing<String>>,
    login_hint: Option<String>,
    state: String,
    nonce: String,
    verifier: Zeroizing<String>,
    url: Secret,
}

/// What the browser brought back.
pub struct Callback {
    code: Secret,
    client_id: String,
    scopes: Vec<String>,
}

impl Callback {
    /// The issued client.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The scopes the callback reported.
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

fn random() -> Result<String, SignInError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    SystemRandom::new()
        .fill(bytes.as_mut())
        .map_err(|_| SignInError::Random)?;
    Ok(B64.encode(bytes.as_ref()))
}

/// The PKCE challenge for `verifier`: base64url SHA-256, no padding.
pub fn code_challenge(verifier: &str) -> String {
    B64.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// Equal without leaking where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// An error code fit to show: OAuth's `snake_case` words, else nothing.
fn code_word(text: &str) -> Option<String> {
    let ok = (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
    ok.then(|| text.to_owned())
}

impl Attempt {
    /// A sign-in for this Mac, with its callback on `127.0.0.1:port`. `returning` is a saved
    /// account signing in again; `None` registers a new one.
    pub fn begin(
        host: &HostId,
        port: u16,
        returning: Option<&Returning>,
    ) -> Result<Self, SignInError> {
        let verifier = Zeroizing::new(random()?);
        let mut attempt = Self {
            host: host.as_str().to_owned(),
            port,
            client_id: returning.map(|r| r.client_id.clone()),
            id_token_hint: returning
                .and_then(|r| r.id_token.as_ref())
                .map(|t| Zeroizing::new(t.expose().to_owned())),
            login_hint: returning.and_then(|r| r.email.clone()),
            state: random()?,
            nonce: random()?,
            verifier,
            url: Secret::token("pending").map_err(|_| SignInError::Random)?,
        };
        attempt.url = attempt.build_url()?;
        Ok(attempt)
    }

    /// The attempt with a fixed nonce, so tests can check pre-signed ID tokens.
    #[cfg(feature = "test-support")]
    pub fn with_nonce_for_tests(mut self, nonce: &str) -> Self {
        nonce.clone_into(&mut self.nonce);
        if let Ok(url) = self.build_url() {
            self.url = url;
        }
        self
    }

    fn build_url(&self) -> Result<Secret, SignInError> {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        match &self.client_id {
            None => {
                q.append_pair("client_id", REGISTER_CLIENT);
                q.append_pair("agent_name_hint", AGENT_NAME);
            }
            Some(client) => {
                q.append_pair("client_id", client);
            }
        }
        q.append_pair("ext_agent_host_id", &self.host);
        if let Some(hint) = &self.id_token_hint {
            q.append_pair("id_token_hint", hint);
        }
        if let Some(email) = &self.login_hint {
            q.append_pair("login_hint", email);
        }
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", &self.redirect_uri());
        q.append_pair("scope", SCOPES);
        q.append_pair("resource", RESOURCE);
        q.append_pair("state", &self.state);
        q.append_pair("nonce", &self.nonce);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("code_challenge", &code_challenge(&self.verifier));
        let query = Zeroizing::new(q.finish());
        // The URL can carry the last ID token, so it's a secret too: never logged or printed.
        Secret::token(format!("{AUTHORIZE_URL}?{}", query.as_str()))
            .map_err(|_| SignInError::Random)
    }

    /// The URL to open in the user's browser.
    pub fn authorize_url(&self) -> &Secret {
        &self.url
    }

    /// The loopback callback the browser returns to.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}{CALLBACK_PATH}", self.port)
    }

    /// Reads the callback's query (with or without its `?`).
    pub fn callback(&self, query: &str) -> Result<Callback, SignInError> {
        let query = query.strip_prefix('?').unwrap_or(query);
        let mut state = None;
        let mut code = None;
        let mut client_id = None;
        let mut scope = None;
        let mut error = None;
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let value = Zeroizing::new(value.into_owned());
            match name.as_ref() {
                "state" => state = Some(value),
                "code" => code = Some(value),
                "client_id" => client_id = Some(value.as_str().to_owned()),
                "scope" => scope = Some(value.as_str().to_owned()),
                "error" => error = Some(value.as_str().to_owned()),
                _ => {}
            }
        }
        if !state.as_deref().is_some_and(|s| same(s, &self.state)) {
            return Err(SignInError::StateMismatch);
        }
        if let Some(error) = error {
            return Err(if error == "access_denied" {
                SignInError::Denied
            } else {
                SignInError::Failed(code_word(&error).unwrap_or_else(|| "unknown".into()))
            });
        }
        let code = code
            .and_then(|c| Secret::token(c.as_str()).ok())
            .ok_or(SignInError::MissingCode)?;
        let client_id = match (&self.client_id, client_id) {
            (None, Some(issued)) if !issued.is_empty() => issued,
            (None, _) => return Err(SignInError::NoClientId),
            (Some(saved), Some(issued)) if &issued != saved => {
                return Err(SignInError::ClientMismatch);
            }
            (Some(saved), _) => saved.clone(),
        };
        let scopes = scope
            .unwrap_or_default()
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(Callback {
            code,
            client_id,
            scopes,
        })
    }

    /// The code exchange's form: the issued client, the code, the verifier, the same redirect.
    pub fn exchange_form(&self, callback: &Callback) -> Form {
        Form::new()
            .with("grant_type", "authorization_code")
            .with("client_id", callback.client_id.clone())
            .with("code", callback.code.expose())
            .with("code_verifier", self.verifier.as_str())
            .with("redirect_uri", self.redirect_uri())
            .with("resource", RESOURCE)
    }

    /// The token endpoint's reply checked into credentials: the ID token verified against
    /// `jwks`, and the plan scope granted. `now_ms` is the current time.
    pub fn finish(
        &self,
        callback: &Callback,
        token_response: &[u8],
        jwks: &[u8],
        now_ms: u64,
    ) -> Result<PlanCredentials, SignInError> {
        let reply: Value =
            serde_json::from_slice(token_response).map_err(|_| SignInError::TokenResponse)?;
        let text = |name: &str| {
            reply
                .get(name)
                .and_then(Value::as_str)
                .ok_or(SignInError::TokenResponse)
        };
        let id_token = text("id_token")?;
        let claims = verify_id_token(id_token, jwks, &callback.client_id, &self.nonce, now_ms)
            .map_err(SignInError::IdToken)?;
        let scopes: Vec<String> = match reply.get("scope").and_then(Value::as_str) {
            Some(granted) => granted
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            None => callback.scopes.clone(),
        };
        if !scopes.iter().any(|s| s == PLAN_SCOPE) {
            return Err(SignInError::PlanNotGranted);
        }
        let expires_in = reply
            .get("expires_in")
            .and_then(Value::as_u64)
            .ok_or(SignInError::TokenResponse)?;
        let token = |raw: &str| Secret::token(raw).map_err(|_| SignInError::TokenResponse);
        Ok(PlanCredentials {
            email: claims.email,
            subject: claims.subject,
            client_id: callback.client_id.clone(),
            host_id: self.host.clone(),
            id_token: token(id_token)?,
            access_token: token(text("access_token")?)?,
            refresh_token: token(text("refresh_token")?)?,
            expires_at_ms: now_ms.saturating_add(expires_in.saturating_mul(1000)),
            scopes,
            saved_at_ms: now_ms,
        })
    }
}

/// The token endpoint's refusal, as a [`SignInError::Exchange`] with only its error code (never
/// its description, which can quote the code).
pub fn exchange_error(status: u16, body: &[u8]) -> SignInError {
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).and_then(code_word));
    SignInError::Exchange(code.unwrap_or_else(|| format!("HTTP {status}")))
}

struct Claims {
    subject: String,
    email: String,
}

fn part(text: &str) -> Result<Value, IdTokenError> {
    let bytes = B64.decode(text).map_err(|_| IdTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IdTokenError::Malformed)
}

/// Checks an ID token as OpenAI's sign-in guide says: an RS256 signature by a published key,
/// then issuer, audience, expiry and nonce, then a subject and an email.
fn verify_id_token(
    token: &str,
    jwks: &[u8],
    client_id: &str,
    nonce: &str,
    now_ms: u64,
) -> Result<Claims, IdTokenError> {
    let mut parts = token.split('.');
    let (Some(head), Some(body), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(IdTokenError::Malformed);
    };
    let header = part(head)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(IdTokenError::Algorithm);
    }
    let kid = header.get("kid").and_then(Value::as_str);
    let keys: Value = serde_json::from_slice(jwks).map_err(|_| IdTokenError::UnknownKey)?;
    let key = keys
        .get("keys")
        .and_then(Value::as_array)
        .and_then(|keys| {
            keys.iter().find(|k| {
                k.get("kty").and_then(Value::as_str) == Some("RSA")
                    && kid.is_some()
                    && k.get("kid").and_then(Value::as_str) == kid
            })
        })
        .ok_or(IdTokenError::UnknownKey)?;
    let component = |name: &str| {
        key.get(name)
            .and_then(Value::as_str)
            .and_then(|v| B64.decode(v).ok())
            .ok_or(IdTokenError::UnknownKey)
    };
    let (n, e) = (component("n")?, component("e")?);
    let signature = B64.decode(sig).map_err(|_| IdTokenError::Malformed)?;
    let signed = &token[..head.len() + 1 + body.len()];
    signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &signature,
        )
        .map_err(|_| IdTokenError::Signature)?;

    let claims = part(body)?;
    if claims.get("iss").and_then(Value::as_str) != Some(ISSUER) {
        return Err(IdTokenError::Issuer);
    }
    let audience_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud == client_id,
        Some(Value::Array(auds)) => auds.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_ok {
        return Err(IdTokenError::Audience);
    }
    let exp = claims
        .get("exp")
        .and_then(Value::as_u64)
        .ok_or(IdTokenError::Expired)?;
    if now_ms / 1000 > exp.saturating_add(LEEWAY_SECS) {
        return Err(IdTokenError::Expired);
    }
    let token_nonce = claims.get("nonce").and_then(Value::as_str).unwrap_or("");
    if !same(token_nonce, nonce) {
        return Err(IdTokenError::Nonce);
    }
    let text = |name: &str| {
        claims
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(IdTokenError::Claims)
    };
    Ok(Claims {
        subject: text("sub")?,
        email: text("email")?,
    })
}

/// The refresh request: the issued client, the saved refresh token, no `scope` (the grant stays).
pub fn refresh_form(credentials: &PlanCredentials) -> Form {
    Form::new()
        .with("grant_type", "refresh_token")
        .with("client_id", credentials.client_id.clone())
        .with("refresh_token", credentials.refresh_token.expose())
        .with("resource", RESOURCE)
}

/// The sign-out request.
pub fn revoke_form(credentials: &PlanCredentials) -> Form {
    Form::new()
        .with("token", credentials.refresh_token.expose())
        .with("token_type_hint", "refresh_token")
        .with("client_id", credentials.client_id.clone())
}

/// A refresh reply applied to `credentials`: the new access token, expiry and scopes, and the
/// rotated refresh token, all together; or why not.
pub fn refreshed(
    credentials: &PlanCredentials,
    status: u16,
    body: &[u8],
    now_ms: u64,
) -> Result<PlanCredentials, RefreshError> {
    if status == 429 || status >= 500 {
        return Err(RefreshError::Unavailable);
    }
    let reply: Value = serde_json::from_slice(body).map_err(|_| match status {
        200..=299 => RefreshError::Unavailable,
        _ => RefreshError::SignInAgain,
    })?;
    if !(200..=299).contains(&status) {
        return Err(match reply.get("error").and_then(Value::as_str) {
            Some("invalid_client") => RefreshError::BadClient,
            // invalid_grant, invalid_refresh_token, token_expired, refresh_token_expired,
            // refresh_token_invalidated, refresh_token_reused, or anything else OpenAI refuses
            // with: the refresh token is no use.
            _ => RefreshError::SignInAgain,
        });
    }
    let text = |name: &str| reply.get(name).and_then(Value::as_str);
    let token = |raw: &str| Secret::token(raw).map_err(|_| RefreshError::Unavailable);
    let access = token(text("access_token").ok_or(RefreshError::Unavailable)?)?;
    let expires_in = reply
        .get("expires_in")
        .and_then(Value::as_u64)
        .ok_or(RefreshError::Unavailable)?;
    let refresh = match text("refresh_token") {
        Some(rotated) => token(rotated)?,
        None => token(credentials.refresh_token.expose())?,
    };
    let id_token = match text("id_token") {
        Some(new) => token(new)?,
        None => token(credentials.id_token.expose())?,
    };
    let scopes = match text("scope") {
        Some(granted) => granted
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        None => credentials.scopes.clone(),
    };
    Ok(PlanCredentials {
        email: credentials.email.clone(),
        subject: credentials.subject.clone(),
        client_id: credentials.client_id.clone(),
        host_id: credentials.host_id.clone(),
        id_token,
        access_token: access,
        refresh_token: refresh,
        expires_at_ms: now_ms.saturating_add(expires_in.saturating_mul(1000)),
        scopes,
        saved_at_ms: now_ms,
    })
}

/// A plan request's error, by OpenAI's plan codes (their "Errors and recovery" page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanFailure {
    /// `subscription_sharing_usage_limit_exceeded`: stop, and point to Manage usage.
    UsageLimit,
    /// `subscription_sharing_user_not_eligible`: not for this user, workspace or policy.
    NotEligible,
    /// `subscription_sharing_unsupported_capability`, with the input it refused.
    Unsupported(Option<String>),
    /// Usage couldn't be checked, or the route is down: retry with backoff.
    Unavailable,
    /// `subscription_sharing_invalid_user`, or a bare 401.
    InvalidUser,
    /// `subscription_sharing_route_not_supported`.
    Route,
    /// `chatpass_v2_scope_not_authorized` or `chatpass_v2_invalid_authorization_context`.
    NotAuthorized,
    /// A bare 403: a policy (such as the region) blocked it.
    Blocked,
    /// Anything else, by status.
    Other(u16),
}

impl PlanFailure {
    /// True when trying again later may work.
    pub fn retry(&self) -> bool {
        matches!(self, Self::Unavailable)
    }

    /// What the card says.
    pub fn message(&self) -> String {
        match self {
            Self::UsageLimit => format!(
                "Usage limit reached on your ChatGPT plan. Manage usage: {MANAGE_USAGE_URL}"
            ),
            Self::NotEligible => "ChatGPT plan usage isn't available on this ChatGPT plan or workspace. Add an OpenAI key to use GPT instead.".into(),
            Self::Unsupported(Some(param)) => {
                format!("Your ChatGPT plan doesn't support part of this request ({param}).")
            }
            Self::Unsupported(None) => "Your ChatGPT plan doesn't support part of this request.".into(),
            Self::Unavailable => "ChatGPT couldn't check your plan's usage. Try again in a minute.".into(),
            Self::InvalidUser => "ChatGPT didn't accept the sign-in. Sign in with ChatGPT again in Settings.".into(),
            Self::Route => "Corral sent a plan request where the plan isn't accepted. This is a bug in Corral.".into(),
            Self::NotAuthorized => "Corral's ChatGPT sign-in can't use the plan. Sign in with ChatGPT again in Settings.".into(),
            Self::Blocked => "ChatGPT blocked this plan request (for example, in this region).".into(),
            Self::Other(status) => format!("ChatGPT returned HTTP {status}."),
        }
    }
}

/// The plan failure an error object names by its code (`{"code": …, "param": …}`), if it is one
/// of the plan's. A request's error reply and a stream's `response.failed` both carry one.
pub fn plan_code(error: &Value) -> Option<PlanFailure> {
    let code = error.get("code").and_then(Value::as_str)?;
    Some(match code {
        "subscription_sharing_usage_limit_exceeded" => PlanFailure::UsageLimit,
        "subscription_sharing_user_not_eligible" => PlanFailure::NotEligible,
        "subscription_sharing_unsupported_capability" => {
            let param = error
                .get("param")
                .and_then(Value::as_str)
                .map(crate::stream::clean)
                .filter(|p| !p.is_empty());
            PlanFailure::Unsupported(param)
        }
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            PlanFailure::Unavailable
        }
        "subscription_sharing_route_not_supported" => PlanFailure::Route,
        "subscription_sharing_invalid_user" => PlanFailure::InvalidUser,
        "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => {
            PlanFailure::NotAuthorized
        }
        _ => return None,
    })
}

/// Reads a plan request's error reply.
pub fn plan_failure(status: u16, body: &[u8]) -> PlanFailure {
    let reply = serde_json::from_slice::<Value>(body).ok();
    let error = reply.as_ref().and_then(|v| v.get("error"));
    match error.and_then(plan_code) {
        Some(failure) => failure,
        None => match status {
            401 => PlanFailure::InvalidUser,
            403 => PlanFailure::Blocked,
            // A plain rate limit, or the direct route down: back off and retry.
            429 | 503 => PlanFailure::Unavailable,
            other => PlanFailure::Other(other),
        },
    }
}

/// The plan's model list (OpenAI's "Models and inference" page for plan usage).
pub const MODELS_URL: &str = "https://api.openai.com/v1/models";
/// At most this many models are kept from the list.
const MAX_MODELS: usize = 64;
/// A slug or name longer than this is cut.
const MAX_MODEL_TEXT: usize = 128;

/// A model the plan lists for choosing: `slug` is sent, `display_name` is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanModel {
    /// What the request's `model` says.
    pub slug: String,
    /// What the picker shows.
    pub display_name: String,
}

/// The model list wasn't one Corral can read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ChatGPT sent a model list Corral couldn't read")]
pub struct BadModelList;

/// The models in a `GET /v1/models` reply that are listed for choosing (`visibility: list`), in
/// the server's order, which is also the plan's default first. Entries without a slug are
/// skipped; one without a display name shows its slug.
pub fn listed_models(body: &[u8]) -> Result<Vec<PlanModel>, BadModelList> {
    let reply = serde_json::from_slice::<Value>(body).map_err(|_| BadModelList)?;
    let entries = reply
        .get("models")
        .and_then(Value::as_array)
        .ok_or(BadModelList)?;
    let text = |entry: &Value, key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(|t| {
                crate::stream::clean(t)
                    .chars()
                    .take(MAX_MODEL_TEXT)
                    .collect::<String>()
            })
            .filter(|t| !t.is_empty())
    };
    let mut models: Vec<PlanModel> = Vec::new();
    for entry in entries {
        if entry.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let Some(slug) = text(entry, "slug") else {
            continue;
        };
        if models.iter().any(|m| m.slug == slug) {
            continue;
        }
        let display_name = text(entry, "display_name").unwrap_or_else(|| slug.clone());
        models.push(PlanModel { slug, display_name });
        if models.len() == MAX_MODELS {
            break;
        }
    }
    Ok(models)
}

const SIGN_IN: &str = "Continue with ChatGPT in Settings to use your ChatGPT plan.";
const SIGNED_OUT: &str = "Your ChatGPT sign-in has ended. Sign in with ChatGPT again in Settings.";

/// The signed-in ChatGPT plan: signs in, hands out a fresh token for each request, and signs out.
pub struct ChatGptPlan {
    store: Arc<dyn PlanStore>,
    http: Arc<dyn FormTransport>,
    clock: Arc<dyn Clock>,
    /// One refresh at a time, so callers don't race a rotating refresh token.
    refresh: tokio::sync::Mutex<()>,
    /// The account whose tokens were cleared, so signing in again reuses its client.
    last: Mutex<Option<PlanAccount>>,
    /// The signed-in account's models, by its subject; fetched again when the account changes.
    models: Mutex<Option<(String, Vec<PlanModel>)>>,
}

impl ChatGptPlan {
    /// A plan whose tokens live in `store`, reaching OpenAI through `http`.
    pub fn new(
        store: Arc<dyn PlanStore>,
        http: Arc<dyn FormTransport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            http,
            clock,
            refresh: tokio::sync::Mutex::new(()),
            last: Mutex::new(None),
            models: Mutex::new(None),
        }
    }

    /// The models the signed-in plan lists, in its order (TP-SUB-06). Fetched with the plan's
    /// token once per account; a change of account, or an empty list, fetches again.
    pub async fn models(&self) -> Result<Vec<PlanModel>, KeyFailure> {
        let subject = match self.store.get_plan() {
            Ok(Some(c)) => c.subject,
            Ok(None) => return Err(KeyFailure::Missing(SIGN_IN.into())),
            Err(e) => {
                return Err(KeyFailure::Internal(format!(
                    "couldn't read the ChatGPT sign-in: {e}"
                )));
            }
        };
        let poisoned = || KeyFailure::Internal("the ChatGPT model list's lock was poisoned".into());
        if let Some((cached, models)) = self.models.lock().map_err(|_| poisoned())?.as_ref()
            && *cached == subject
        {
            return Ok(models.clone());
        }
        let token = self.token().await?;
        let (status, body) = self
            .http
            .get_authorized(MODELS_URL, &token)
            .await
            .map_err(|_| {
                KeyFailure::Unavailable(
                    "Couldn't reach ChatGPT for your plan's models. Try again in a minute.".into(),
                )
            })?;
        if status != 200 {
            let failure = plan_failure(status, &body);
            return Err(match failure {
                PlanFailure::InvalidUser | PlanFailure::NotAuthorized => {
                    KeyFailure::Missing(failure.message())
                }
                PlanFailure::UsageLimit
                | PlanFailure::NotEligible
                | PlanFailure::Unsupported(_)
                | PlanFailure::Unavailable
                | PlanFailure::Route
                | PlanFailure::Blocked
                | PlanFailure::Other(_) => KeyFailure::Unavailable(failure.message()),
            });
        }
        let models = listed_models(&body).map_err(|e| KeyFailure::Unavailable(e.to_string()))?;
        // An empty list isn't kept, so the next run asks again.
        if !models.is_empty() {
            *self.models.lock().map_err(|_| poisoned())? = Some((subject, models.clone()));
        }
        Ok(models)
    }

    /// The account signed in now, if any.
    pub fn signed_in(&self) -> Result<Option<PlanAccount>, SecretError> {
        Ok(self.store.get_plan()?.as_ref().map(PlanAccount::of))
    }

    /// The account signed in now, or the last one whose tokens were cleared.
    pub fn account(&self) -> Result<Option<PlanAccount>, SecretError> {
        if let Some(c) = self.store.get_plan()? {
            return Ok(Some(PlanAccount::of(&c)));
        }
        let last = self.last.lock().map_err(|_| SecretError::Poisoned)?;
        Ok(last.clone())
    }

    fn remember(&self, account: PlanAccount) {
        // A poisoned lock only loses the hint for the next sign-in, which then registers anew.
        if let Ok(mut last) = self.last.lock() {
            *last = Some(account);
        }
    }

    /// Finishes `attempt` with the browser's `callback`: exchanges the code, verifies the ID
    /// token against OpenAI's keys, and saves the tokens only if the plan scope was granted.
    pub async fn sign_in(
        &self,
        attempt: &Attempt,
        callback: &Callback,
    ) -> Result<PlanAccount, SignInError> {
        let form = attempt.exchange_form(callback);
        let (status, reply) = self
            .http
            .post_form(TOKEN_URL, &form)
            .await
            .map_err(|e| SignInError::Network(e.to_string()))?;
        let reply = Zeroizing::new(reply);
        if status != 200 {
            return Err(exchange_error(status, &reply));
        }
        let (status, jwks) = self
            .http
            .get(JWKS_URL)
            .await
            .map_err(|_| SignInError::Keys)?;
        if status != 200 {
            return Err(SignInError::Keys);
        }
        let credentials = attempt.finish(callback, &reply, &jwks, self.clock.now_ms())?;
        self.store.set_plan(&credentials)?;
        Ok(PlanAccount::of(&credentials))
    }

    /// Signs out: stops using the tokens, revokes them at OpenAI (retrying a few times if it
    /// can't be reached), and deletes them here either way.
    pub async fn sign_out(&self) -> Result<SignedOut, SecretError> {
        let _refreshing = self.refresh.lock().await;
        let Some(credentials) = self.store.get_plan()? else {
            return Ok(SignedOut { revoked: true });
        };
        let form = revoke_form(&credentials);
        let mut revoked = false;
        for attempt in 1..=REVOKE_ATTEMPTS {
            match self.http.post_form(REVOKE_URL, &form).await {
                Ok((200..=299, _)) => {
                    revoked = true;
                    break;
                }
                Ok((status, _)) if status < 500 => break,
                Ok(_) | Err(_) if attempt < REVOKE_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                }
                Ok(_) | Err(_) => {}
            }
        }
        self.store.delete_plan()?;
        self.remember(PlanAccount::of(&credentials));
        Ok(SignedOut { revoked })
    }

    async fn token(&self) -> Result<Secret, KeyFailure> {
        let fresh = |c: &PlanCredentials| {
            c.expires_at_ms > self.clock.now_ms().saturating_add(REFRESH_AHEAD_MS)
        };
        let read = || {
            self.store.get_plan().map_err(|e| {
                KeyFailure::Internal(format!("couldn't read the ChatGPT sign-in: {e}"))
            })
        };
        let Some(credentials) = read()? else {
            return Err(KeyFailure::Missing(SIGN_IN.into()));
        };
        if fresh(&credentials) {
            return Ok(credentials.access_token);
        }
        let _refreshing = self.refresh.lock().await;
        // Another caller may have refreshed while this one waited.
        let Some(credentials) = read()? else {
            return Err(KeyFailure::Missing(SIGN_IN.into()));
        };
        if fresh(&credentials) {
            return Ok(credentials.access_token);
        }
        let unavailable = || {
            KeyFailure::Unavailable(
                "Couldn't reach ChatGPT to renew the sign-in. Try again in a minute.".into(),
            )
        };
        let (status, reply) = self
            .http
            .post_form(TOKEN_URL, &refresh_form(&credentials))
            .await
            .map_err(|_| unavailable())?;
        let reply = Zeroizing::new(reply);
        match refreshed(&credentials, status, &reply, self.clock.now_ms()) {
            Ok(renewed) => {
                self.store
                    .set_plan(&renewed)
                    .map_err(|e| KeyFailure::Internal(format!("couldn't save the ChatGPT sign-in: {e}")))?;
                Ok(renewed.access_token)
            }
            Err(RefreshError::SignInAgain) => {
                self.store
                    .delete_plan()
                    .map_err(|e| KeyFailure::Internal(format!("couldn't clear the ChatGPT sign-in: {e}")))?;
                self.remember(PlanAccount::of(&credentials));
                Err(KeyFailure::Missing(SIGNED_OUT.into()))
            }
            Err(RefreshError::BadClient) => Err(KeyFailure::Missing(
                "ChatGPT no longer accepts Corral's registration. Sign in with ChatGPT again in Settings.".into(),
            )),
            Err(RefreshError::Unavailable) => Err(unavailable()),
        }
    }
}

impl KeySource for ChatGptPlan {
    /// The plan's access token, renewed first when it has less than five minutes left. The
    /// provider is always OpenAI's; the plan has no other.
    fn key(&self, _provider: Provider) -> BoxFuture<'_, Result<Secret, KeyFailure>> {
        Box::pin(self.token())
    }
}

/// Builds the driver for one run, given the plan's configuration with the model chosen.
pub type PlanBuild = Arc<dyn Fn(ResponsesConfig) -> Arc<dyn RunDriver> + Send + Sync>;

/// GPT on the signed-in ChatGPT plan (TP-SUB-07): each run asks the plan for its models, uses
/// the chosen one while the plan lists it (else the plan's first), and runs through the plan's
/// route. No sign-in, or no models, fails the run before any request.
pub struct PlanAgent {
    plan: Arc<ChatGptPlan>,
    preferred: Option<String>,
    build: PlanBuild,
    pauses: bool,
}

impl PlanAgent {
    /// An agent on `plan` that prefers the model `preferred`, builds each run's driver with
    /// `build`, and offers Pause when that driver stops at safe points (`pauses`).
    pub fn new(
        plan: Arc<ChatGptPlan>,
        preferred: Option<String>,
        build: PlanBuild,
        pauses: bool,
    ) -> Self {
        Self {
            plan,
            preferred,
            build,
            pauses,
        }
    }
}

/// The run's model: the preferred one while the plan lists it, else the plan's first.
fn chosen<'a>(models: &'a [PlanModel], preferred: Option<&str>) -> Option<&'a PlanModel> {
    preferred
        .and_then(|slug| models.iter().find(|m| m.slug == slug))
        .or_else(|| models.first())
}

impl RunDriver for PlanAgent {
    fn pauses(&self, _agent: &AgentId) -> bool {
        self.pauses
    }

    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let plan = self.plan.clone();
        let preferred = self.preferred.clone();
        let build = self.build.clone();
        Box::pin(async move {
            let models = tokio::select! {
                biased;
                // The supervisor logs run.cancelled.
                () = cancel.cancelled() => return,
                models = plan.models() => models,
            };
            let models = match models {
                Ok(models) => models,
                Err(KeyFailure::Missing(message) | KeyFailure::Unavailable(message)) => {
                    return fail_run(&sink, FailureReason::Provider, message);
                }
                Err(KeyFailure::Internal(message)) => {
                    return fail_run(&sink, FailureReason::Internal, message);
                }
            };
            let Some(model) = chosen(&models, preferred.as_deref()) else {
                return fail_run(
                    &sink,
                    FailureReason::Provider,
                    "Your ChatGPT plan offers no models right now. Try again later.".into(),
                );
            };
            let driver = build(ResponsesConfig::chatgpt_plan(model.slug.clone()));
            driver.run(request, sink, cancel).await;
        })
    }
}
