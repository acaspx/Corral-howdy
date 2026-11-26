//! TP-SUB-04: signing in with ChatGPT from the app (ADR-017, `docs/04-test-plan.md`).
//!
//! The real loopback listener on 127.0.0.1 is driven by a fake browser over TCP: it reads the
//! authorization URL Corral opened and comes back to the callback the way a browser redirect
//! does. OpenAI's endpoints are scripted and its ID tokens synthetic (the core's fixtures), so
//! nothing reaches the network (AGENTS.md rule 7).

// Shared with the core's tests, which use every token; the app's sign-in needs only the good one.
#[allow(dead_code)]
#[path = "../../../../crates/corral-adapters/tests/fixtures/chatgpt/id_tokens.rs"]
mod id_tokens;

use corral_adapters::chatgpt::{Attempt, ChatGptPlan, HostId, Returning};
use corral_adapters::transport::{BoxFuture, Form, FormTransport, TransportError};
use corral_core::clock::ManualClock;
use corral_desktop_lib::chatgpt_signin::{
    Browser, ChatGptSignIn, SignInFiles, callback_target, sign_in_page,
};
use corral_secrets::{MemoryPlanStore, PlanStore, Secret};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLIENT: &str = "oaiapp_corral_test";
const NONCE: &str = "nonce-fixture-0001";
const NOW_MS: u64 = 1_790_001_000_000;
const ACCESS: &str = "plan-access-Zq81mXv3TtYb7cLp0wRn"; // gitleaks:allow (made-up test token)
const REFRESH: &str = "plan-refresh-Ux5bWk0aGm7rCz2fYq9"; // gitleaks:allow (made-up test token)
const CODE: &str = "auth-code-Pf6hQa2yKu0eMs9tBg3"; // gitleaks:allow (made-up test code)
const GRANTED: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

fn jwks() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../crates/corral-adapters/tests/fixtures/chatgpt/jwks.json");
    std::fs::read(path).expect("jwks fixture")
}

fn token_response() -> Vec<u8> {
    serde_json::json!({
        "id_token": id_tokens::VALID, "access_token": ACCESS, "refresh_token": REFRESH,
        "token_type": "Bearer", "expires_in": 3600, "scope": GRANTED,
    })
    .to_string()
    .into_bytes()
}

/// A fresh, empty folder for one test's files.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corral-signin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // A leftover from an earlier run, if any.
    std::fs::create_dir_all(&dir).expect("test folder");
    dir
}

// ---- A scripted OpenAI ---------------------------------------------------------------------

/// What was sent: the URL and the form's fields.
type Sent = (String, Vec<(String, String)>);

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
        Box::pin(async move { self.next(url, fields) })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move { self.next(url, Vec::new()) })
    }
}

// ---- A fake browser ------------------------------------------------------------------------

/// What the browser does once Corral opens the sign-in page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    /// A request for something else on the same port (a favicon).
    Stray,
    /// A callback carrying a state that isn't the attempt's.
    WrongState,
    /// The user allowed it: the real callback.
    Allow,
    /// The user said no.
    Deny,
    /// The user closed the tab: nothing comes back.
    Nothing,
}

#[derive(Default)]
struct Seen {
    urls: Vec<String>,
    replies: Vec<String>,
}

struct FakeBrowser {
    visits: Vec<Visit>,
    seen: Arc<Mutex<Seen>>,
}

impl FakeBrowser {
    fn doing(visits: &[Visit]) -> (Arc<Self>, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        (
            Arc::new(Self {
                visits: visits.to_vec(),
                seen: seen.clone(),
            }),
            seen,
        )
    }
}

fn param(url: &str, name: &str) -> Option<String> {
    let (_, query) = url.split_once('?')?;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.to_owned())
    })
}

async fn get(port: u16, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the callback listener accepts");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request sent");
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.expect("a reply");
    reply
}

impl Browser for FakeBrowser {
    fn open(&self, url: &Secret) -> Result<(), String> {
        let url = url.expose().to_owned();
        self.seen.lock().expect("seen").urls.push(url.clone());
        let redirect = param(&url, "redirect_uri").expect("a redirect URI");
        let port: u16 = redirect
            .split("127.0.0.1%3A")
            .nth(1)
            .and_then(|rest| rest.split('%').next())
            .and_then(|p| p.parse().ok())
            .expect("a loopback port");
        let state = param(&url, "state").expect("a state");
        let visits = self.visits.clone();
        let seen = self.seen.clone();
        tokio::spawn(async move {
            for visit in visits {
                let path = match visit {
                    Visit::Stray => "/favicon.ico".to_owned(),
                    Visit::WrongState => {
                        format!("/auth/callback?code={CODE}&state=forged&client_id={CLIENT}")
                    }
                    Visit::Allow => format!(
                        "/auth/callback?code={CODE}&state={state}&client_id={CLIENT}&scope=openid"
                    ),
                    Visit::Deny => format!("/auth/callback?error=access_denied&state={state}"),
                    Visit::Nothing => return,
                };
                let reply = get(port, &path).await;
                seen.lock().expect("seen").replies.push(reply);
            }
        });
        Ok(())
    }
}

/// The app's sign-in, with the fixtures' nonce so their pre-signed ID tokens check out.
fn signin(
    dir: &Path,
    store: Arc<MemoryPlanStore>,
    http: Arc<Scripted>,
    browser: Arc<FakeBrowser>,
) -> ChatGptSignIn {
    let plan = Arc::new(ChatGptPlan::new(
        store,
        http,
        Arc::new(ManualClock::at(NOW_MS)),
    ));
    ChatGptSignIn::new(plan, SignInFiles::new(dir.to_owned()), browser).with_begin(Arc::new(
        |host: &HostId, port: u16, returning: Option<&Returning>| {
            Attempt::begin(host, port, returning).map(|a: Attempt| a.with_nonce_for_tests(NONCE))
        },
    ))
}

// ---- TP-SUB-04 -----------------------------------------------------------------------------

#[test]
fn tp_sub_04_this_macs_host_id_is_made_once_and_kept() {
    let dir = folder("host");
    let first = SignInFiles::new(dir.clone()).host_id().expect("made");
    let again = SignInFiles::new(dir.clone()).host_id().expect("read back");
    assert_eq!(first, again, "the same Mac sends the same host ID");
    assert!(first.as_str().starts_with("urn:uuid:"));

    std::fs::write(dir.join("host.json"), "{not json").expect("write");
    assert!(
        SignInFiles::new(dir).host_id().is_err(),
        "an unreadable host file is reported, not silently replaced"
    );
}

#[test]
fn tp_sub_04_only_a_callback_to_the_callback_path_is_read() {
    assert_eq!(
        callback_target("GET /auth/callback?code=a&state=b HTTP/1.1"),
        Some("code=a&state=b")
    );
    assert_eq!(callback_target("GET /auth/callback HTTP/1.1"), Some(""));
    for other in [
        "GET /favicon.ico HTTP/1.1",
        "POST /auth/callback?code=a HTTP/1.1",
        "GET /auth/callbackX?code=a HTTP/1.1",
        "GET http://evil.test/auth/callback?code=a HTTP/1.1",
        "garbage",
        "",
    ] {
        assert_eq!(callback_target(other), None, "{other:?}");
    }
}

#[test]
fn tp_sub_04_the_browser_page_is_static_and_locked_down() {
    for (ok, page) in [
        (true, sign_in_page(true, "Signed in")),
        (false, sign_in_page(false, "Cancelled")),
    ] {
        let head = page.split("\r\n\r\n").next().expect("headers");
        assert!(
            head.starts_with(if ok { "HTTP/1.1 200" } else { "HTTP/1.1 400" }),
            "{head}"
        );
        assert!(
            head.contains("Content-Security-Policy: default-src 'none'"),
            "{head}"
        );
        assert!(head.contains("Cache-Control: no-store"), "{head}");
        assert!(head.contains("Referrer-Policy: no-referrer"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        assert!(!page.contains("<script"), "no script on the page");
    }
}

#[tokio::test]
async fn tp_sub_04_signing_in_opens_the_browser_and_waits_for_the_real_callback() {
    let dir = folder("full");
    let store = Arc::new(MemoryPlanStore::default());
    let http = Scripted::replying(vec![(200, token_response()), (200, jwks())]);
    let (browser, seen) = FakeBrowser::doing(&[Visit::Stray, Visit::WrongState, Visit::Allow]);
    let signin = signin(&dir, store.clone(), http.clone(), browser);

    assert!(!signin.status().expect("status").signed_in);
    let status = signin.sign_in().await.expect("signed in");
    assert!(status.signed_in);
    assert_eq!(status.email.as_deref(), Some("anton@example.test"));
    assert!(
        status.show_welcome,
        "the first sign-in shows the welcome once"
    );

    // The sign-in ends once it has answered; the browser may still be reading that answer.
    for _ in 0..200 {
        if seen.lock().expect("seen").replies.len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let seen = seen.lock().expect("seen");
    let url = &seen.urls[0];
    assert!(
        url.starts_with("https://auth.openai.com/api/accounts/authorize?"),
        "{url}"
    );
    assert_eq!(
        param(url, "client_id").as_deref(),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        param(url, "id_token_hint"),
        None,
        "never on the command line"
    );
    assert!(
        seen.replies[0].starts_with("HTTP/1.1 404"),
        "a stray request is turned away"
    );
    assert!(
        seen.replies[1].starts_with("HTTP/1.1 400"),
        "a callback with someone else's state is refused, and the wait goes on"
    );
    assert!(
        seen.replies[2].starts_with("HTTP/1.1 200"),
        "{}",
        seen.replies[2]
    );
    for reply in &seen.replies {
        assert!(!reply.contains(CODE), "the page never echoes the code");
    }
    assert_eq!(http.sent().len(), 2, "one code exchange, one key fetch");
    assert_eq!(
        store
            .get_plan()
            .expect("store")
            .expect("saved")
            .access_token
            .expose(),
        ACCESS
    );

    // The account file names the account and its client, and holds no token.
    let account = std::fs::read_to_string(dir.join("account.json")).expect("account file");
    assert!(account.contains(CLIENT) && account.contains("anton@example.test"));
    for token in [ACCESS, REFRESH, id_tokens::VALID] {
        assert!(
            !account.contains(&token[..12]),
            "a token in the account file"
        );
    }

    let status = signin.welcomed().expect("welcomed");
    assert!(status.signed_in && !status.show_welcome);
    assert!(!signin.status().expect("status").show_welcome, "only once");
}

#[tokio::test]
async fn tp_sub_04_signing_out_keeps_the_registration_for_next_time() {
    let dir = folder("again");
    let store = Arc::new(MemoryPlanStore::default());
    let http = Scripted::replying(vec![
        (200, token_response()),
        (200, jwks()),
        (200, Vec::new()), // revoke
        (200, token_response()),
        (200, jwks()),
    ]);
    let (browser, seen) = FakeBrowser::doing(&[Visit::Allow]);
    let signin = signin(&dir, store.clone(), http.clone(), browser);
    signin.sign_in().await.expect("signed in");

    let out = signin.sign_out().await.expect("signed out");
    assert!(out.revoked);
    assert!(!out.status.signed_in);
    assert!(store.get_plan().expect("store").is_none());
    assert_eq!(
        http.sent()[2].0,
        "https://auth.openai.com/api/accounts/oauth/revoke"
    );

    signin.sign_in().await.expect("signed in again");
    let seen = seen.lock().expect("seen");
    let again = &seen.urls[1];
    assert_eq!(
        param(again, "client_id").as_deref(),
        Some(CLIENT),
        "the saved client"
    );
    assert_eq!(param(again, "agent_name_hint"), None);
    assert_eq!(
        param(again, "login_hint").as_deref(),
        Some("anton%40example.test")
    );
    assert_eq!(param(again, "id_token_hint"), None);
    assert_eq!(
        param(again, "ext_agent_host_id"),
        param(&seen.urls[0], "ext_agent_host_id"),
        "the same host ID"
    );
    assert!(
        !signin.status().expect("status").show_welcome,
        "the welcome isn't shown on later sign-ins"
    );
}

#[tokio::test]
async fn tp_sub_04_a_denied_sign_in_saves_nothing() {
    let dir = folder("denied");
    let store = Arc::new(MemoryPlanStore::default());
    let (browser, _) = FakeBrowser::doing(&[Visit::Deny]);
    let signin = signin(&dir, store.clone(), Scripted::replying(vec![]), browser);
    let error = signin.sign_in().await.expect_err("denied");
    assert!(error.contains("cancelled"), "{error}");
    assert!(store.get_plan().expect("store").is_none());
    assert!(!dir.join("account.json").exists());
}

#[tokio::test(start_paused = true)]
async fn tp_sub_04_a_sign_in_nobody_finishes_times_out() {
    let dir = folder("timeout");
    let (browser, _) = FakeBrowser::doing(&[Visit::Nothing]);
    let signin = signin(
        &dir,
        Arc::new(MemoryPlanStore::default()),
        Scripted::replying(vec![]),
        browser,
    )
    .with_timeout(Duration::from_secs(300));
    let error = signin.sign_in().await.expect_err("timed out");
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn tp_sub_04_cancelling_ends_a_sign_in_still_waiting() {
    let dir = folder("cancel");
    let store = Arc::new(MemoryPlanStore::default());
    let (waiting, _) = FakeBrowser::doing(&[Visit::Nothing]);
    let signin = Arc::new(signin(
        &dir,
        store.clone(),
        Scripted::replying(vec![]),
        waiting,
    ));
    let pending = tokio::spawn({
        let signin = signin.clone();
        async move { signin.sign_in().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The user closed the tab and pressed Cancel (or Continue with ChatGPT again).
    signin.cancel();
    let error = pending.await.expect("joined").expect_err("cancelled");
    assert!(error.contains("cancelled"), "{error}");
    assert!(store.get_plan().expect("store").is_none());
}
