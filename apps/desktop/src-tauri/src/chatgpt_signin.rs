//! Signing in with ChatGPT from the app (ADR-017, TP-SUB-04). The decisions are the core's
//! ([`corral_adapters::chatgpt`]): what the request carries, what the callback must match, which
//! ID token is good, where the tokens go. This module is the plumbing around them:
//!
//! - **This Mac's host ID** is made once and kept in `<app data>/chatgpt/host.json`. An unreadable
//!   file is reported, never silently replaced.
//! - **The account file** (`account.json`) keeps the email, the issued client and the subject so
//!   the next sign-in reuses the registration, plus whether the welcome was seen. Never a token:
//!   those are only in the Keychain item.
//! - **The browser** is opened with `/usr/bin/open` on the authorization URL. The URL never
//!   carries `id_token_hint`, since a command line isn't private; the email is enough of a hint.
//! - **The callback** is a listener on `127.0.0.1`, on a port the OS picks, that reads only
//!   `GET /auth/callback`. Anything else is turned away, and so is a callback with the wrong
//!   state, while the wait goes on: a stale tab or another page can't end the sign-in. Each
//!   answer is a static page with no script, `default-src 'none'`, `no-store` and no referrer.
//! - **One sign-in at a time.** A new one, Cancel, or sign-out ends the one still waiting; after
//!   five minutes it gives up.

use corral_adapters::chatgpt::{
    Attempt, CALLBACK_PATH, ChatGptPlan, HostId, MANAGE_USAGE_URL, Returning, SignInError,
};
use corral_secrets::Secret;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::State;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// How long a sign-in waits for the browser.
const TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// The most of a request the listener reads: its request line and headers.
const REQUEST_LIMIT: usize = 8 * 1024;
/// How long one connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens a URL in the user's browser.
pub trait Browser: Send + Sync {
    /// Opens `url`.
    fn open(&self, url: &Secret) -> Result<(), String>;
}

/// The default browser, through `/usr/bin/open`.
pub struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &Secret) -> Result<(), String> {
        open_url(url.expose())
    }
}

fn open_url(url: &str) -> Result<(), String> {
    match std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("couldn't open the browser ({status})")),
        Err(e) => Err(format!("couldn't open the browser: {e}")),
    }
}

/// Makes the sign-in attempt; tests swap in one with a fixed nonce.
pub type Begin =
    Arc<dyn Fn(&HostId, u16, Option<&Returning>) -> Result<Attempt, SignInError> + Send + Sync>;

/// The account the next sign-in reuses. No tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedAccount {
    /// The account's email.
    pub email: String,
    /// The client OpenAI issued when Corral registered.
    pub client_id: String,
    /// The verified subject.
    pub subject: String,
    /// Whether the welcome was seen.
    #[serde(default)]
    pub welcomed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostFile {
    host_id: String,
}

/// The sign-in's files in the app's data folder.
pub struct SignInFiles {
    dir: PathBuf,
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let show = path.display();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("couldn't make {show}'s folder: {e}"))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("couldn't write {show}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("couldn't protect {show}: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("couldn't save {show}: {e}"))
}

impl SignInFiles {
    /// Files in `dir` (`<app data>/chatgpt` in the app).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// This Mac's host ID, made and saved the first time.
    pub fn host_id(&self) -> Result<HostId, String> {
        let path = self.dir.join("host.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<HostFile>(&text)
                .ok()
                .and_then(|f| HostId::parse(&f.host_id).ok())
                .ok_or_else(|| {
                    format!(
                        "{} is unreadable; remove it and sign in again to make a new one",
                        path.display()
                    )
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let host = HostId::generate().map_err(|e| e.to_string())?;
                let text = serde_json::json!({ "host_id": host.as_str() }).to_string();
                write_private(&path, &text)?;
                Ok(host)
            }
            Err(e) => Err(format!("couldn't read {}: {e}", path.display())),
        }
    }

    /// The saved account, if Corral ever registered.
    pub fn account(&self) -> Result<Option<SavedAccount>, String> {
        let path = self.dir.join("account.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|_| {
                format!(
                    "{} is unreadable; remove it and sign in again",
                    path.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("couldn't read {}: {e}", path.display())),
        }
    }

    fn save_account(&self, account: &SavedAccount) -> Result<(), String> {
        let text = serde_json::json!({
            "email": account.email,
            "client_id": account.client_id,
            "subject": account.subject,
            "welcomed": account.welcomed,
        })
        .to_string();
        write_private(&self.dir.join("account.json"), &text)
    }
}

/// What the window shows of the ChatGPT plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptStatus {
    /// Signed in now.
    pub signed_in: bool,
    /// The account's email, while signed in.
    pub email: Option<String>,
    /// Show the one-time welcome (after the first sign-in, until Got it).
    pub show_welcome: bool,
}

/// What signing out did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptSignedOut {
    /// False when ChatGPT didn't confirm the revocation; the tokens are gone from this Mac anyway.
    pub revoked: bool,
    /// The status after.
    pub status: ChatGptStatus,
}

/// The app's ChatGPT sign-in.
pub struct ChatGptSignIn {
    plan: Arc<ChatGptPlan>,
    files: SignInFiles,
    browser: Arc<dyn Browser>,
    begin: Begin,
    timeout: Duration,
    waiting: Mutex<Option<Arc<Notify>>>,
}

impl ChatGptSignIn {
    /// A sign-in for `plan`, keeping its files in `files` and opening pages with `browser`.
    pub fn new(plan: Arc<ChatGptPlan>, files: SignInFiles, browser: Arc<dyn Browser>) -> Self {
        Self {
            plan,
            files,
            browser,
            begin: Arc::new(|host: &HostId, port: u16, returning: Option<&Returning>| {
                Attempt::begin(host, port, returning)
            }),
            timeout: TIMEOUT,
            waiting: Mutex::new(None),
        }
    }

    /// The sign-in with another way to make attempts (tests fix the nonce).
    pub fn with_begin(mut self, begin: Begin) -> Self {
        self.begin = begin;
        self
    }

    /// The sign-in giving up after `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The plan, for the agents that run on it.
    pub fn plan(&self) -> &Arc<ChatGptPlan> {
        &self.plan
    }

    /// Whether an account is signed in, and whether to welcome it.
    pub fn status(&self) -> Result<ChatGptStatus, String> {
        let account = self.plan.signed_in().map_err(|e| e.to_string())?;
        let welcomed = self.files.account()?.is_none_or(|a| a.welcomed);
        Ok(ChatGptStatus {
            signed_in: account.is_some(),
            show_welcome: account.is_some() && !welcomed,
            email: account.map(|a| a.email),
        })
    }

    /// Ends the sign-in still waiting, if any.
    pub fn cancel(&self) {
        // A poisoned lock means a panic mid-swap; the waiting sign-in still times out.
        if let Ok(mut waiting) = self.waiting.lock()
            && let Some(notify) = waiting.take()
        {
            notify.notify_one();
        }
    }

    /// Opens the browser at OpenAI's sign-in and waits for its callback; ends any sign-in still
    /// waiting first.
    pub async fn sign_in(&self) -> Result<ChatGptStatus, String> {
        let notify = Arc::new(Notify::new());
        if let Ok(mut waiting) = self.waiting.lock()
            && let Some(old) = waiting.replace(Arc::clone(&notify))
        {
            old.notify_one();
        }
        let result = tokio::select! {
            biased;
            () = notify.notified() => Err(SignInError::Denied.to_string()),
            done = tokio::time::timeout(self.timeout, self.wait_for_browser()) => match done {
                Ok(result) => result,
                Err(_) => Err("ChatGPT sign-in timed out. Continue with ChatGPT to try again.".into()),
            },
        };
        if let Ok(mut waiting) = self.waiting.lock()
            && waiting.as_ref().is_some_and(|w| Arc::ptr_eq(w, &notify))
        {
            *waiting = None;
        }
        result
    }

    async fn wait_for_browser(&self) -> Result<ChatGptStatus, String> {
        let host = self.files.host_id()?;
        let saved = self.files.account()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| format!("couldn't listen for the sign-in: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("couldn't listen for the sign-in: {e}"))?
            .port();
        let returning = saved.as_ref().map(|a| Returning {
            client_id: a.client_id.clone(),
            id_token: None,
            email: Some(a.email.clone()),
        });
        let attempt = (self.begin)(&host, port, returning.as_ref()).map_err(|e| e.to_string())?;
        self.browser.open(attempt.authorize_url())?;
        loop {
            let (mut stream, _) = listener
                .accept()
                .await
                .map_err(|e| format!("the sign-in listener failed: {e}"))?;
            let Some(head) = read_head(&mut stream).await else {
                continue;
            };
            let Some(query) = callback_target(&head) else {
                answer(&mut stream, &not_found_page()).await;
                continue;
            };
            let callback = match attempt.callback(query) {
                Ok(callback) => callback,
                Err(SignInError::StateMismatch) => {
                    let page = sign_in_page(
                        false,
                        "This sign-in link is out of date. Go back to Corral and choose Continue with ChatGPT again.",
                    );
                    answer(&mut stream, &page).await;
                    continue;
                }
                Err(e) => {
                    let page = sign_in_page(
                        false,
                        "Sign-in didn't finish. You can close this tab and go back to Corral.",
                    );
                    answer(&mut stream, &page).await;
                    return Err(e.to_string());
                }
            };
            return match self.plan.sign_in(&attempt, &callback).await {
                Ok(account) => {
                    // The welcome is for the first sign-in only.
                    let welcomed = saved.is_some();
                    self.files.save_account(&SavedAccount {
                        email: account.email,
                        client_id: account.client_id,
                        subject: account.subject,
                        welcomed,
                    })?;
                    let page = sign_in_page(
                        true,
                        "You're signed in. You can close this tab and go back to Corral.",
                    );
                    answer(&mut stream, &page).await;
                    self.status()
                }
                Err(e) => {
                    let page = sign_in_page(
                        false,
                        "Sign-in didn't finish. You can close this tab and go back to Corral.",
                    );
                    answer(&mut stream, &page).await;
                    Err(e.to_string())
                }
            };
        }
    }

    /// Signs out: ends a waiting sign-in, revokes the tokens at OpenAI and deletes them here.
    /// The account file stays, so the next sign-in reuses the registration.
    pub async fn sign_out(&self) -> Result<ChatGptSignedOut, String> {
        self.cancel();
        let out = self.plan.sign_out().await.map_err(|e| e.to_string())?;
        Ok(ChatGptSignedOut {
            revoked: out.revoked,
            status: self.status()?,
        })
    }

    /// The welcome was seen.
    pub fn welcomed(&self) -> Result<ChatGptStatus, String> {
        if let Some(mut account) = self.files.account()? {
            account.welcomed = true;
            self.files.save_account(&account)?;
        }
        self.status()
    }
}

/// The query of a `GET /auth/callback` request line, or `None` for anything else.
pub fn callback_target(head: &str) -> Option<&str> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !version.starts_with("HTTP/") {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (path == CALLBACK_PATH).then_some(query)
}

/// A request's line and headers, up to [`REQUEST_LIMIT`] bytes and [`READ_TIMEOUT`].
async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let read = async {
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < REQUEST_LIMIT {
            let n = stream.read(&mut buf).await.ok()?;
            if n == 0 {
                break;
            }
            head.extend_from_slice(&buf[..n]);
        }
        Some(())
    };
    tokio::time::timeout(READ_TIMEOUT, read).await.ok()??;
    String::from_utf8(head).ok()
}

/// Sends `page` and closes. A browser that went away is fine: the sign-in goes on without it.
async fn answer(stream: &mut TcpStream, page: &str) {
    if stream.write_all(page.as_bytes()).await.is_ok() {
        // Best effort: the page is a courtesy, the sign-in's outcome doesn't depend on it.
        let _ = stream.shutdown().await;
    }
}

fn page(status: &str, title: &str, message: &str) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title></head>\
         <body style=\"font:16px -apple-system,system-ui,sans-serif;margin:4rem auto;max-width:28rem\">\
         <h1 style=\"font-size:20px\">{}</h1><p>{}</p></body></html>",
        escape(title),
        escape(title),
        escape(message)
    );
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The page the browser shows after the callback: 200 when signed in, 400 otherwise.
pub fn sign_in_page(ok: bool, message: &str) -> String {
    if ok {
        page("200 OK", "Signed in to Corral", message)
    } else {
        page("400 Bad Request", "Corral didn't sign in", message)
    }
}

fn not_found_page() -> String {
    page(
        "404 Not Found",
        "Not found",
        "This page is only for signing in to Corral.",
    )
}

// Async so a Keychain prompt never blocks the main thread.

/// Command: whether a ChatGPT account is signed in.
#[tauri::command(async)]
pub fn chatgpt_status(signin: State<'_, Arc<ChatGptSignIn>>) -> Result<ChatGptStatus, String> {
    signin.status()
}

/// Command: sign in through the browser.
#[tauri::command]
pub async fn chatgpt_sign_in(
    signin: State<'_, Arc<ChatGptSignIn>>,
) -> Result<ChatGptStatus, String> {
    let signin = Arc::clone(signin.inner());
    signin.sign_in().await
}

/// Command: stop waiting for the browser.
#[tauri::command]
pub fn chatgpt_cancel_sign_in(signin: State<'_, Arc<ChatGptSignIn>>) {
    signin.cancel();
}

/// Command: sign out.
#[tauri::command]
pub async fn chatgpt_sign_out(
    signin: State<'_, Arc<ChatGptSignIn>>,
) -> Result<ChatGptSignedOut, String> {
    let signin = Arc::clone(signin.inner());
    signin.sign_out().await
}

/// Command: the welcome was seen.
#[tauri::command(async)]
pub fn chatgpt_welcomed(signin: State<'_, Arc<ChatGptSignIn>>) -> Result<ChatGptStatus, String> {
    signin.welcomed()
}

/// Command: open ChatGPT's usage settings, at its one fixed URL.
#[tauri::command(async)]
pub fn open_manage_usage() -> Result<(), String> {
    open_url(MANAGE_USAGE_URL)
}
