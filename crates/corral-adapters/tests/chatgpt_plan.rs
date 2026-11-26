//! ChatGPT plan usage through Sign in with ChatGPT (ADR-017): TP-SUB-01..03's core halves
//! (`docs/04-test-plan.md`). The sign-in follows OpenAI's open-source flow
//! (developers.openai.com/siwc/token-sharing-open-source), the tokens live only in a
//! [`PlanStore`], and runs on the plan follow its Responses route. Nothing here touches the
//! network (AGENTS.md rule 7): OpenAI's endpoints are a scripted [`FormTransport`], ID tokens are
//! synthetic (`fixtures/chatgpt/make.py`), and model calls replay cassettes.

#[path = "fixtures/chatgpt/id_tokens.rs"]
mod id_tokens;

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::chatgpt::{
    self, Attempt, ChatGptPlan, HostId, IdTokenError, PlanFailure, RefreshError, Returning,
    SignInError,
};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_adapters::stream::{KeyFailure, KeySource};
use corral_adapters::transport::{BoxFuture, Form, FormTransport, TransportError};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_secrets::{MemoryPlanStore, PlanCredentials, PlanStore, Provider, Secret};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The fixtures' issued client, nonce and times (make.py).
const CLIENT: &str = "oaiapp_corral_test";
const NONCE: &str = "nonce-fixture-0001";
/// 1,000 s after the fixtures' `iat`, well before their `exp`.
const NOW_MS: u64 = 1_790_001_000_000;
const ACCESS: &str = "plan-access-Zq81mXv3TtYb7cLp0wRn"; // gitleaks:allow (made-up test token)
const ACCESS_2: &str = "plan-access-second-Hn4KdS9ePq2vJw6"; // gitleaks:allow (made-up test token)
const REFRESH: &str = "plan-refresh-Ux5bWk0aGm7rCz2fYq9"; // gitleaks:allow (made-up test token)
const REFRESH_2: &str = "plan-refresh-second-Lo3iTe8sAv1nDx4"; // gitleaks:allow (made-up test token)
const CODE: &str = "auth-code-Pf6hQa2yKu0eMs9tBg3"; // gitleaks:allow (made-up test code)

fn jwks() -> Vec<u8> {
    std::fs::read(fixture_dir().join("jwks.json")).expect("jwks fixture")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chatgpt")
}

fn host() -> HostId {
    HostId::parse("urn:uuid:3f2b8c1e-9d4a-4e7b-8a6f-1c2d3e4f5a6b").expect("a valid host ID")
}

/// A query string from pairs, percent-encoding values the way a browser redirect does.
fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", v.replace(' ', "%20").replace('+', "%2B")))
        .collect::<Vec<_>>()
        .join("&")
}

/// The authorization URL's query parameters, decoded.
fn params(url: &str) -> Vec<(String, String)> {
    let (_, q) = url.split_once('?').expect("a query string");
    q.split('&')
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k.to_owned(), decode(v))
        })
        .collect()
}

fn param(url: &str, name: &str) -> Option<String> {
    params(url)
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
}

fn decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("hex");
                out.push(u8::from_str_radix(hex, 16).expect("hex digit"));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("utf-8")
}

/// An attempt whose nonce is the fixtures' (tests can't know a random one in advance).
fn attempt(returning: Option<&Returning>) -> Attempt {
    Attempt::begin(&host(), 1455, returning)
        .expect("an attempt")
        .with_nonce_for_tests(NONCE)
}

fn token_response(id_token: &str, scope: &str) -> Vec<u8> {
    serde_json::json!({
        "id_token": id_token,
        "access_token": ACCESS,
        "refresh_token": REFRESH,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": scope,
    })
    .to_string()
    .into_bytes()
}

const GRANTED: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const NO_PLAN: &str = "openid profile email";

fn signed_in(expires_at_ms: u64) -> PlanCredentials {
    PlanCredentials {
        email: "anton@example.test".into(),
        subject: "user-test-subject".into(),
        client_id: CLIENT.into(),
        host_id: host().as_str().into(),
        id_token: Secret::token(id_tokens::VALID).expect("id token"),
        access_token: Secret::token(ACCESS).expect("access token"),
        refresh_token: Secret::token(REFRESH).expect("refresh token"),
        expires_at_ms,
        scopes: GRANTED.split(' ').map(String::from).collect(),
        saved_at_ms: NOW_MS,
    }
}

// ---- A scripted OpenAI ---------------------------------------------------------------------

/// What was sent: the URL and the form's fields.
type Sent = (String, Vec<(String, String)>);

/// Replies to form POSTs and GETs in order, and keeps what was sent.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<(u16, Vec<u8>)>>,
    sent: Mutex<Vec<Sent>>,
}

impl Scripted {
    fn replying(replies: Vec<(u16, Vec<u8>)>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            sent: Mutex::default(),
        })
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().expect("sent").clone()
    }

    fn next(
        &self,
        url: &str,
        fields: Vec<(String, String)>,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        self.sent
            .lock()
            .expect("sent")
            .push((url.to_owned(), fields));
        self.replies
            .lock()
            .expect("replies")
            .pop_front()
            .ok_or_else(|| TransportError::Exhausted(format!("no reply scripted for {url}")))
    }
}

impl FormTransport for Scripted {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        let fields = form
            .fields()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Box::pin(async move {
            // A real reply takes time: let other callers run meanwhile (TP-SUB-02's race).
            tokio::task::yield_now().await;
            self.next(url, fields)
        })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.next(url, Vec::new())
        })
    }
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn plan(store: Arc<MemoryPlanStore>, http: Arc<Scripted>, clock: Arc<ManualClock>) -> ChatGptPlan {
    ChatGptPlan::new(store, http, clock)
}

// ---- TP-SUB-01: the sign-in ----------------------------------------------------------------

#[test]
fn tp_sub_01_a_host_id_is_an_opaque_uuid_urn_made_once() {
    let a = HostId::generate().expect("random host ID");
    let b = HostId::generate().expect("random host ID");
    assert_ne!(a, b, "each host gets its own");
    let text = a.as_str();
    assert!(text.starts_with("urn:uuid:"), "{text}");
    let uuid = &text["urn:uuid:".len()..];
    assert_eq!(uuid.len(), 36);
    assert_eq!(uuid.as_bytes()[14], b'4', "a version-4 UUID: {uuid}");
    assert!(
        matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
        "RFC 4122 variant: {uuid}"
    );
    assert_eq!(HostId::parse(text).expect("round-trips"), a);
    for bad in ["", "anton@example.test", "urn:uuid:not-a-uuid", "user-123"] {
        assert!(
            matches!(HostId::parse(bad), Err(SignInError::HostId)),
            "{bad:?} must be refused"
        );
    }
    assert!(HostId::parse("urn:ietf:params:oauth:jwk-thumbprint:sha-256:NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs").is_ok());
}

#[test]
fn tp_sub_01_the_first_sign_in_registers_corral_with_fresh_state_nonce_and_pkce() {
    let a = Attempt::begin(&host(), 1455, None).expect("an attempt");
    let b = Attempt::begin(&host(), 1455, None).expect("an attempt");
    let url = a.authorize_url();
    let url = url.expose();
    assert!(
        url.starts_with("https://auth.openai.com/api/accounts/authorize?"),
        "{url}"
    );
    assert_eq!(
        param(url, "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(param(url, "agent_name_hint").as_deref(), Some("Corral"));
    assert_eq!(
        param(url, "ext_agent_host_id").as_deref(),
        Some(host().as_str())
    );
    assert_eq!(param(url, "response_type").as_deref(), Some("code"));
    assert_eq!(
        param(url, "redirect_uri").as_deref(),
        Some("http://127.0.0.1:1455/auth/callback"),
        "a loopback callback on 127.0.0.1, never localhost"
    );
    assert_eq!(a.redirect_uri(), "http://127.0.0.1:1455/auth/callback");
    assert_eq!(param(url, "scope").as_deref(), Some(GRANTED));
    assert_eq!(
        param(url, "resource").as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(param(url, "code_challenge_method").as_deref(), Some("S256"));
    for name in ["id_token_hint", "login_hint", "prompt"] {
        assert_eq!(
            param(url, name),
            None,
            "{name} isn't sent on a first sign-in"
        );
    }

    let other = b.authorize_url();
    let other = other.expose();
    for name in ["state", "nonce", "code_challenge"] {
        let mine = param(url, name).expect(name);
        assert!(mine.len() >= 43, "{name} carries at least 256 bits: {mine}");
        assert_ne!(
            Some(mine),
            param(other, name),
            "{name} is fresh for every attempt"
        );
    }
    let challenge = param(url, "code_challenge").expect("challenge");
    assert!(
        challenge
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "base64url without padding: {challenge}"
    );
}

#[test]
fn tp_sub_01_signing_in_again_uses_the_saved_client_and_drops_the_name_hint() {
    let returning = Returning {
        client_id: CLIENT.into(),
        id_token: Some(Secret::token(id_tokens::VALID).expect("id token")),
        email: Some("anton@example.test".into()),
    };
    let a = Attempt::begin(&host(), 51234, Some(&returning)).expect("an attempt");
    let url = a.authorize_url();
    let url = url.expose();
    assert_eq!(param(url, "client_id").as_deref(), Some(CLIENT));
    assert_eq!(param(url, "agent_name_hint"), None);
    assert_eq!(
        param(url, "id_token_hint").as_deref(),
        Some(id_tokens::VALID)
    );
    assert_eq!(
        param(url, "login_hint").as_deref(),
        Some("anton@example.test")
    );
    assert_eq!(
        param(url, "ext_agent_host_id").as_deref(),
        Some(host().as_str())
    );
    assert_eq!(
        a.redirect_uri(),
        "http://127.0.0.1:51234/auth/callback",
        "only the port varies"
    );
    assert_eq!(
        format!("{:?}", a.authorize_url()),
        "Secret(redacted)",
        "the URL can carry an ID token, so it never prints"
    );
}

#[test]
fn tp_sub_01_a_callback_must_carry_the_attempts_state() {
    let a = attempt(None);
    let state = param(a.authorize_url().expose(), "state").expect("state");

    let wrong = query(&[
        ("code", CODE),
        ("state", "someone-elses-state"),
        ("client_id", CLIENT),
    ]);
    assert!(matches!(
        a.callback(&wrong),
        Err(SignInError::StateMismatch)
    ));
    let missing = query(&[("code", CODE), ("client_id", CLIENT)]);
    assert!(matches!(
        a.callback(&missing),
        Err(SignInError::StateMismatch)
    ));

    let denied = query(&[("error", "access_denied"), ("state", &state)]);
    assert!(matches!(a.callback(&denied), Err(SignInError::Denied)));
    let denied_forged = query(&[("error", "access_denied"), ("state", "forged")]);
    assert!(
        matches!(a.callback(&denied_forged), Err(SignInError::StateMismatch)),
        "a denial is believed only with the right state"
    );

    let no_code = query(&[("state", &state), ("client_id", CLIENT)]);
    assert!(matches!(
        a.callback(&no_code),
        Err(SignInError::MissingCode)
    ));
    let no_client = query(&[("code", CODE), ("state", &state)]);
    assert!(
        matches!(a.callback(&no_client), Err(SignInError::NoClientId)),
        "a new registration must return its issued client ID"
    );
    let ok = query(&[
        ("code", CODE),
        ("state", &state),
        ("client_id", CLIENT),
        ("scope", GRANTED),
    ]);
    let callback = a.callback(&ok).expect("a good callback");
    assert_eq!(callback.client_id(), CLIENT);
}

#[test]
fn tp_sub_01_a_returning_sign_in_refuses_a_different_client() {
    let returning = Returning {
        client_id: CLIENT.into(),
        id_token: None,
        email: None,
    };
    let a = attempt(Some(&returning));
    let state = param(a.authorize_url().expose(), "state").expect("state");
    let other = query(&[
        ("code", CODE),
        ("state", &state),
        ("client_id", "oaiapp_someone_else"),
    ]);
    assert!(matches!(
        a.callback(&other),
        Err(SignInError::ClientMismatch)
    ));
    let omitted = query(&[("code", CODE), ("state", &state)]);
    assert_eq!(
        a.callback(&omitted)
            .expect("the saved client is kept")
            .client_id(),
        CLIENT
    );
}

#[test]
fn tp_sub_01_the_code_is_exchanged_with_the_issued_client_and_the_verifier() {
    let a = attempt(None);
    let state = param(a.authorize_url().expose(), "state").expect("state");
    let cb = a
        .callback(&query(&[
            ("code", CODE),
            ("state", &state),
            ("client_id", CLIENT),
        ]))
        .expect("callback");
    let form = a.exchange_form(&cb);
    assert_eq!(form.get("grant_type"), Some("authorization_code"));
    assert_eq!(
        form.get("client_id"),
        Some(CLIENT),
        "never dynamic_agent_client"
    );
    assert_eq!(form.get("code"), Some(CODE));
    assert_eq!(
        form.get("redirect_uri"),
        Some("http://127.0.0.1:1455/auth/callback")
    );
    assert_eq!(form.get("resource"), Some("https://api.openai.com/v1"));
    assert_eq!(form.get("client_secret"), None);
    let verifier = form.get("code_verifier").expect("verifier");
    assert!(
        (43..=128).contains(&verifier.len()),
        "RFC 7636 length: {}",
        verifier.len()
    );
    assert_eq!(
        chatgpt::code_challenge(verifier),
        param(a.authorize_url().expose(), "code_challenge").expect("challenge"),
        "the verifier is the one the challenge came from"
    );
    assert_eq!(format!("{form:?}"), "Form(redacted)");
}

fn finish(id_token: &str, scope: &str) -> Result<PlanCredentials, SignInError> {
    let a = attempt(None);
    let state = param(a.authorize_url().expose(), "state").expect("state");
    let cb = a
        .callback(&query(&[
            ("code", CODE),
            ("state", &state),
            ("client_id", CLIENT),
        ]))
        .expect("callback");
    a.finish(&cb, &token_response(id_token, scope), &jwks(), NOW_MS)
}

#[test]
fn tp_sub_01_a_verified_id_token_and_the_plan_scope_make_the_credentials() {
    let c = finish(id_tokens::VALID, GRANTED).expect("signed in");
    assert_eq!(c.email, "anton@example.test");
    assert_eq!(c.subject, "user-test-subject");
    assert_eq!(c.client_id, CLIENT);
    assert_eq!(c.host_id, host().as_str());
    assert_eq!(c.access_token.expose(), ACCESS);
    assert_eq!(c.refresh_token.expose(), REFRESH);
    assert_eq!(c.expires_at_ms, NOW_MS + 3_600_000);
    assert_eq!(c.saved_at_ms, NOW_MS);
    assert!(c.scopes.iter().any(|s| s == chatgpt::PLAN_SCOPE));
}

#[test]
fn tp_sub_01_without_the_plan_scope_nothing_is_kept() {
    assert!(matches!(
        finish(id_tokens::VALID, NO_PLAN),
        Err(SignInError::PlanNotGranted)
    ));
}

#[test]
fn tp_sub_01_every_bad_id_token_is_refused() {
    let cases = [
        (id_tokens::WRONG_ISSUER, IdTokenError::Issuer),
        (id_tokens::WRONG_AUDIENCE, IdTokenError::Audience),
        (id_tokens::EXPIRED, IdTokenError::Expired),
        (id_tokens::WRONG_NONCE, IdTokenError::Nonce),
        (id_tokens::BAD_SIGNATURE, IdTokenError::Signature),
        (id_tokens::ALG_NONE, IdTokenError::Algorithm),
        (id_tokens::ALG_HS256, IdTokenError::Algorithm),
        (id_tokens::UNKNOWN_KID, IdTokenError::UnknownKey),
        (id_tokens::NO_EMAIL, IdTokenError::Claims),
        ("not.a-token", IdTokenError::Malformed),
    ];
    for (token, expected) in cases {
        match finish(token, GRANTED) {
            Err(SignInError::IdToken(e)) => assert_eq!(e, expected, "{expected:?}"),
            other => panic!("{expected:?} must be refused, got {other:?}"),
        }
    }
}

#[test]
fn tp_sub_01_a_token_endpoint_error_keeps_only_its_code() {
    let body =
        br#"{"error":"invalid_grant","error_description":"code auth-code-Pf6hQa2y was used"}"#;
    let SignInError::Exchange(code) = chatgpt::exchange_error(400, body) else {
        panic!("expected an exchange error");
    };
    assert_eq!(code, "invalid_grant");
    let SignInError::Exchange(code) = chatgpt::exchange_error(502, b"<html>bad gateway</html>")
    else {
        panic!("expected an exchange error");
    };
    assert_eq!(code, "HTTP 502", "no body text is kept");
}

#[tokio::test]
async fn tp_sub_01_signing_in_saves_the_credentials_only_to_the_plan_store() {
    let store = Arc::new(MemoryPlanStore::default());
    let http = Scripted::replying(vec![
        (200, token_response(id_tokens::VALID, GRANTED)),
        (200, jwks()),
    ]);
    let clock = Arc::new(ManualClock::at(NOW_MS));
    let plan = plan(store.clone(), http.clone(), clock);
    let a = attempt(None);
    let state = param(a.authorize_url().expose(), "state").expect("state");
    let cb = a
        .callback(&query(&[
            ("code", CODE),
            ("state", &state),
            ("client_id", CLIENT),
        ]))
        .expect("callback");
    let account = plan.sign_in(&a, &cb).await.expect("signed in");
    assert_eq!(account.email, "anton@example.test");
    assert_eq!(account.client_id, CLIENT);

    let sent = http.sent();
    assert_eq!(sent[0].0, chatgpt::TOKEN_URL);
    assert_eq!(sent[1].0, chatgpt::JWKS_URL);
    let saved = store.get_plan().expect("store").expect("saved");
    assert_eq!(saved.access_token.expose(), ACCESS);

    // Without the plan scope, the store stays empty.
    let store = Arc::new(MemoryPlanStore::default());
    let http = Scripted::replying(vec![
        (200, token_response(id_tokens::VALID, NO_PLAN)),
        (200, jwks()),
    ]);
    let plan2 = ChatGptPlan::new(store.clone(), http, Arc::new(ManualClock::at(NOW_MS)));
    assert!(matches!(
        plan2.sign_in(&a, &cb).await,
        Err(SignInError::PlanNotGranted)
    ));
    assert!(store.get_plan().expect("store").is_none());
}

// ---- TP-SUB-02: the tokens -----------------------------------------------------------------

#[test]
fn tp_sub_02_credentials_never_print_their_tokens() {
    let c = signed_in(NOW_MS + 3_600_000);
    let shown = format!("{c:?}");
    for secret in [ACCESS, REFRESH, id_tokens::VALID] {
        assert!(
            !shown.contains(&secret[..12]),
            "Debug shows a token: {shown}"
        );
    }
    assert!(
        shown.contains("anton@example.test"),
        "the account is fine to show"
    );
    let record = c.to_record();
    let back = PlanCredentials::from_record(&record).expect("round-trips");
    assert_eq!(back.access_token.expose(), ACCESS);
    assert_eq!(back.refresh_token.expose(), REFRESH);
    assert_eq!(back.expires_at_ms, c.expires_at_ms);
    assert!(PlanCredentials::from_record("{\"email\":1}").is_err());
}

#[tokio::test]
async fn tp_sub_02_a_fresh_token_is_used_as_it_is() {
    let store = Arc::new(MemoryPlanStore::default());
    store
        .set_plan(&signed_in(NOW_MS + 3_600_000))
        .expect("store");
    let http = Scripted::replying(vec![]);
    let plan = plan(store, http.clone(), Arc::new(ManualClock::at(NOW_MS)));
    let key = plan.key(Provider::OpenAi).await.expect("a token");
    assert_eq!(key.expose(), ACCESS);
    assert!(
        http.sent().is_empty(),
        "no refresh while the token has time left"
    );
}

#[tokio::test]
async fn tp_sub_02_a_token_near_expiry_is_refreshed_at_call_time_and_rotated() {
    let store = Arc::new(MemoryPlanStore::default());
    store.set_plan(&signed_in(NOW_MS + 60_000)).expect("store");
    let refreshed = serde_json::json!({
        "access_token": ACCESS_2, "refresh_token": REFRESH_2, "token_type": "Bearer",
        "expires_in": 3600, "scope": GRANTED,
    });
    let http = Scripted::replying(vec![(200, refreshed.to_string().into_bytes())]);
    let plan = plan(
        store.clone(),
        http.clone(),
        Arc::new(ManualClock::at(NOW_MS)),
    );

    let key = plan.key(Provider::OpenAi).await.expect("a token");
    assert_eq!(key.expose(), ACCESS_2);
    let (url, fields) = &http.sent()[0];
    assert_eq!(url, chatgpt::TOKEN_URL);
    assert_eq!(field(fields, "grant_type"), Some("refresh_token"));
    assert_eq!(field(fields, "client_id"), Some(CLIENT));
    assert_eq!(field(fields, "refresh_token"), Some(REFRESH));
    assert_eq!(field(fields, "resource"), Some("https://api.openai.com/v1"));
    assert_eq!(field(fields, "scope"), None, "the grant is kept as it is");

    let saved = store.get_plan().expect("store").expect("saved");
    assert_eq!(saved.access_token.expose(), ACCESS_2);
    assert_eq!(
        saved.refresh_token.expose(),
        REFRESH_2,
        "the rotated refresh token replaces the old"
    );
    assert_eq!(saved.expires_at_ms, NOW_MS + 3_600_000);
    assert_eq!(saved.email, "anton@example.test", "the account stays");
}

#[tokio::test]
async fn tp_sub_02_refreshes_are_serialized() {
    let store = Arc::new(MemoryPlanStore::default());
    store.set_plan(&signed_in(NOW_MS + 1_000)).expect("store");
    let refreshed = serde_json::json!({
        "access_token": ACCESS_2, "refresh_token": REFRESH_2, "expires_in": 3600, "scope": GRANTED,
    });
    let http = Scripted::replying(vec![(200, refreshed.to_string().into_bytes())]);
    let plan = Arc::new(plan(store, http.clone(), Arc::new(ManualClock::at(NOW_MS))));
    let (a, b) = tokio::join!(plan.key(Provider::OpenAi), plan.key(Provider::OpenAi));
    assert_eq!(a.expect("a").expose(), ACCESS_2);
    assert_eq!(b.expect("b").expose(), ACCESS_2);
    assert_eq!(http.sent().len(), 1, "one refresh, not one per caller");
}

#[tokio::test]
async fn tp_sub_02_a_dead_refresh_token_clears_the_tokens_and_asks_to_sign_in_again() {
    for code in [
        "invalid_grant",
        "refresh_token_expired",
        "refresh_token_reused",
    ] {
        let store = Arc::new(MemoryPlanStore::default());
        store.set_plan(&signed_in(NOW_MS - 1)).expect("store");
        let body = format!(r#"{{"error":"{code}"}}"#).into_bytes();
        let http = Scripted::replying(vec![(400, body)]);
        let plan = plan(store.clone(), http, Arc::new(ManualClock::at(NOW_MS)));
        match plan.key(Provider::OpenAi).await {
            Err(KeyFailure::Missing(message)) => {
                assert!(message.contains("Sign in with ChatGPT again"), "{message}")
            }
            other => panic!("{code}: expected sign in again, got {other:?}"),
        }
        assert!(
            store.get_plan().expect("store").is_none(),
            "{code}: the tokens are cleared"
        );
        let account = plan.account().expect("account");
        assert_eq!(
            account.map(|a| a.client_id).as_deref(),
            Some(CLIENT),
            "the registration is kept, so signing in again reuses it"
        );
    }
}

#[tokio::test]
async fn tp_sub_02_a_network_failure_keeps_the_tokens() {
    let store = Arc::new(MemoryPlanStore::default());
    store.set_plan(&signed_in(NOW_MS - 1)).expect("store");
    let http = Scripted::replying(vec![(503, b"{}".to_vec())]);
    let plan = plan(store.clone(), http, Arc::new(ManualClock::at(NOW_MS)));
    assert!(matches!(
        plan.key(Provider::OpenAi).await,
        Err(KeyFailure::Unavailable(_))
    ));
    assert!(
        store.get_plan().expect("store").is_some(),
        "a passing outage isn't a sign-out"
    );
}

#[test]
fn tp_sub_02_refresh_errors_are_sorted_by_whether_signing_in_again_helps() {
    let c = signed_in(NOW_MS);
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ] {
        let body = format!(r#"{{"error":"{code}"}}"#);
        assert!(
            matches!(
                chatgpt::refreshed(&c, 400, body.as_bytes(), NOW_MS),
                Err(RefreshError::SignInAgain)
            ),
            "{code}"
        );
    }
    assert!(matches!(
        chatgpt::refreshed(&c, 401, br#"{"error":"invalid_client"}"#, NOW_MS),
        Err(RefreshError::BadClient)
    ));
    for status in [500, 502, 503] {
        assert!(matches!(
            chatgpt::refreshed(&c, status, b"oops", NOW_MS),
            Err(RefreshError::Unavailable)
        ));
    }
}

#[tokio::test]
async fn tp_sub_02_signing_out_revokes_at_openai_then_deletes_the_tokens() {
    let store = Arc::new(MemoryPlanStore::default());
    store
        .set_plan(&signed_in(NOW_MS + 3_600_000))
        .expect("store");
    let http = Scripted::replying(vec![(200, Vec::new())]);
    let plan = plan(
        store.clone(),
        http.clone(),
        Arc::new(ManualClock::at(NOW_MS)),
    );
    let out = plan.sign_out().await.expect("signed out");
    assert!(out.revoked);
    let (url, fields) = &http.sent()[0];
    assert_eq!(url, chatgpt::REVOKE_URL);
    assert_eq!(field(fields, "token"), Some(REFRESH));
    assert_eq!(field(fields, "token_type_hint"), Some("refresh_token"));
    assert_eq!(field(fields, "client_id"), Some(CLIENT));
    assert!(store.get_plan().expect("store").is_none());
    assert!(matches!(
        plan.key(Provider::OpenAi).await,
        Err(KeyFailure::Missing(_))
    ));
}

#[tokio::test]
async fn tp_sub_02_unconfirmed_revocation_still_deletes_and_says_so() {
    let store = Arc::new(MemoryPlanStore::default());
    store
        .set_plan(&signed_in(NOW_MS + 3_600_000))
        .expect("store");
    let http = Scripted::replying(vec![
        (503, Vec::new()),
        (503, Vec::new()),
        (503, Vec::new()),
    ]);
    let plan = plan(store.clone(), http, Arc::new(ManualClock::at(NOW_MS)));
    let out = plan.sign_out().await.expect("signed out");
    assert!(
        !out.revoked,
        "the window tells the user to disconnect Corral in ChatGPT's settings"
    );
    assert!(store.get_plan().expect("store").is_none());
}

// ---- TP-SUB-03: runs on the plan -------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log lock").clone()
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    fn failure(&self) -> RunFailed {
        let terminal = self.events().last().cloned().expect("events");
        let EventBody::RunFailed(f) = terminal.body else {
            panic!("expected run.failed, got {:?}", terminal.kind());
        };
        f
    }
}

/// A key source that hands out one fixed plan token.
struct FixedToken;

impl KeySource for FixedToken {
    fn key(&self, _provider: Provider) -> BoxFuture<'_, Result<Secret, KeyFailure>> {
        Box::pin(async { Secret::token(ACCESS).map_err(|e| KeyFailure::Internal(e.to_string())) })
    }
}

struct Run {
    log: Arc<MemLog>,
    transport: Arc<CassetteTransport>,
}

async fn run(cassette_name: &str, keys: Arc<dyn KeySource>) -> Run {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cassettes/openai-plan")
        .join(cassette_name);
    let transport = Arc::new(CassetteTransport::new(
        Cassette::load(&path).expect("cassette loads"),
        4096,
    ));
    let driver = ResponsesDriver::with_keys(
        transport.clone(),
        keys,
        ResponsesConfig::chatgpt_plan("gpt-test"),
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("gpt"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("the run ends");
    log.assert_legal();
    Run { log, transport }
}

#[tokio::test(start_paused = true)]
async fn tp_sub_03_a_plan_request_follows_the_plan_route() {
    let r = run("text-ok.json", Arc::new(FixedToken)).await;
    let seen = r.transport.seen();
    assert_eq!(seen.len(), 1);
    let sent = &seen[0];
    assert_eq!(sent.url, "https://api.openai.com/v1/responses");
    assert_eq!(sent.bearer_last4, &ACCESS[ACCESS.len() - 4..]);
    assert_eq!(sent.body["store"], false);
    assert_eq!(sent.body["stream"], true);
    assert!(
        sent.body["input"].is_array(),
        "the plan wants input as an array: {}",
        sent.body
    );
    for omitted in [
        "max_output_tokens",
        "previous_response_id",
        "background",
        "metadata",
        "temperature",
        "user",
    ] {
        assert!(
            sent.body.get(omitted).is_none(),
            "{omitted} isn't accepted on the plan route"
        );
    }
    let terminal = r.log.events().last().cloned().expect("events");
    assert!(
        matches!(terminal.body, EventBody::RunCompleted(_)),
        "{:?}",
        terminal.kind()
    );
    let spent: u64 = r
        .log
        .events()
        .iter()
        .filter_map(|e| {
            if let EventBody::UsageReported(u) = &e.body {
                Some(u.cost_micro_usd)
            } else {
                None
            }
        })
        .sum();
    assert_eq!(spent, 0, "plan usage costs nothing in dollars");
}

#[tokio::test(start_paused = true)]
async fn tp_sub_03_the_usage_limit_stops_the_run_with_manage_usage_and_no_retry() {
    let r = run("usage-limit.json", Arc::new(FixedToken)).await;
    assert_eq!(r.transport.seen().len(), 1, "a usage limit isn't retried");
    let f = r.log.failure();
    assert_eq!(f.reason, FailureReason::Provider);
    assert!(f.message.contains("Usage limit reached"), "{}", f.message);
    assert!(
        f.message.contains(chatgpt::MANAGE_USAGE_URL),
        "{}",
        f.message
    );
}

#[tokio::test(start_paused = true)]
async fn tp_sub_03_an_unavailable_plan_is_retried_and_then_answers() {
    let r = run("unavailable-then-ok.json", Arc::new(FixedToken)).await;
    assert_eq!(r.transport.seen().len(), 2);
    let terminal = r.log.events().last().cloned().expect("events");
    assert!(matches!(terminal.body, EventBody::RunCompleted(_)));
}

#[tokio::test(start_paused = true)]
async fn tp_sub_03_an_ineligible_plan_or_unsupported_input_says_which() {
    let r = run("not-eligible.json", Arc::new(FixedToken)).await;
    assert_eq!(r.transport.seen().len(), 1, "not retried");
    let message = r.log.failure().message;
    assert!(
        message.contains("isn't available on this ChatGPT plan"),
        "{message}"
    );

    let r = run("unsupported.json", Arc::new(FixedToken)).await;
    assert_eq!(
        r.transport.seen().len(),
        1,
        "the same body isn't sent again"
    );
    let message = r.log.failure().message;
    assert!(
        message.contains("doesn't support") && message.contains("tools[0]"),
        "{message}"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_sub_03_without_a_sign_in_nothing_is_sent() {
    let store = Arc::new(MemoryPlanStore::default());
    let plan: Arc<dyn KeySource> = Arc::new(ChatGptPlan::new(
        store,
        Scripted::replying(vec![]),
        Arc::new(ManualClock::at(NOW_MS)),
    ));
    let r = run("text-ok.json", plan).await;
    assert!(r.transport.seen().is_empty());
    assert!(
        r.log.failure().message.contains("Continue with ChatGPT"),
        "{}",
        r.log.failure().message
    );
}

#[test]
fn tp_sub_03_plan_errors_are_read_by_their_codes() {
    let body =
        |code: &str| format!(r#"{{"error":{{"code":"{code}","message":"x","param":null}}}}"#);
    let cases = [
        (
            429,
            "subscription_sharing_usage_limit_exceeded",
            PlanFailure::UsageLimit,
        ),
        (
            403,
            "subscription_sharing_user_not_eligible",
            PlanFailure::NotEligible,
        ),
        (
            503,
            "subscription_sharing_usage_unavailable",
            PlanFailure::Unavailable,
        ),
        (
            503,
            "subscription_sharing_user_unavailable",
            PlanFailure::Unavailable,
        ),
        (
            403,
            "subscription_sharing_route_not_supported",
            PlanFailure::Route,
        ),
        (
            401,
            "subscription_sharing_invalid_user",
            PlanFailure::InvalidUser,
        ),
        (
            403,
            "chatpass_v2_scope_not_authorized",
            PlanFailure::NotAuthorized,
        ),
        (
            403,
            "chatpass_v2_invalid_authorization_context",
            PlanFailure::NotAuthorized,
        ),
    ];
    for (status, code, expected) in cases {
        assert_eq!(
            chatgpt::plan_failure(status, body(code).as_bytes()),
            expected,
            "{code}"
        );
    }
    assert_eq!(
        chatgpt::plan_failure(400, br#"{"error":{"code":"subscription_sharing_unsupported_capability","param":"tools[0]"}}"#),
        PlanFailure::Unsupported(Some("tools[0]".into()))
    );
    // Admission errors before a stream opens can be `{"detail": ...}`, sorted by status alone.
    assert_eq!(
        chatgpt::plan_failure(401, br#"{"detail":"nope"}"#),
        PlanFailure::InvalidUser
    );
    assert_eq!(
        chatgpt::plan_failure(403, br#"{"detail":"region"}"#),
        PlanFailure::Blocked
    );
    assert_eq!(
        chatgpt::plan_failure(503, b"<html>"),
        PlanFailure::Unavailable
    );
    assert!(PlanFailure::Unavailable.retry());
    assert!(!PlanFailure::UsageLimit.retry());
}

#[test]
fn tp_sub_03_the_plan_route_is_off_for_keys() {
    let xai = ResponsesConfig::xai("grok-test");
    let body = xai.request_body("hi");
    assert!(
        body.get("max_output_tokens").is_some(),
        "a keyed request still carries its cap"
    );
    let plan = ResponsesConfig::chatgpt_plan("gpt-test");
    assert_eq!(plan.provider, Provider::OpenAi);
    assert_eq!(plan.url(), "https://api.openai.com/v1/responses");
    let body = plan.request_body("hi");
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"], "hi");
}
