//! The e2e build (Spike S2 step 4, TP-E2E-01..04): the app the nightly drives with XCUITest,
//! built with `--features native,e2e` and its own bundle ID (`tauri.e2e.conf.json`), so its data
//! folder is never a real install's (the nightly also gives it its own home folder). Everything around the core is swapped for a stand-in, and the
//! core itself is unchanged: runs still go through the supervisor and the budget meter, and the
//! scripts call no tools, so nothing needs the gate (AGENTS.md rules 3 and 4).
//!
//! - **Providers:** every catalog agent is a [`FakeDriver`] running [`script`]: it answers
//!   [`ANSWER`] after a wait long enough to watch the ring and the hover card.
//! - **Keys:** in memory, never the Keychain, and any key is accepted without asking its provider.
//! - **Network:** none. ChatGPT's sign-in transport refuses every request, and logos are a fixed
//!   picture.
//!
//! [`MARKER`] is printed when the app starts. `scripts/ci/no_e2e_in_release.sh` fails a release
//! binary that contains it (TP-CI-17..19), so this build can't ship by mistake.

use crate::keys::{KeyChecker, Keys};
use corral_adapters::catalog::agent_for;
use corral_adapters::chatgpt::ChatGptPlan;
use corral_adapters::fake::{FakeDriver, FakeScript};
use corral_adapters::logos::{LogoError, LogoFetcher, LogoSource};
use corral_adapters::transport::{BoxFuture, Form, FormTransport, TransportError};
use corral_adapters::verify::ModelsProbe;
use corral_core::budget::Price;
use corral_core::clock::SystemClock;
use corral_core::ids::AgentId;
use corral_core::supervisor::RunDriver;
use corral_secrets::{MemoryPlanStore, MemoryStore, Provider};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// What the e2e build says when it starts, and what the release check looks for.
pub const MARKER: &str =
    "corral-e2e-build: fake providers, keys in memory, no network. Never ship this build.";

/// Every scripted agent's answer, to any task.
pub const ANSWER: &str = "Four. This is the e2e build's scripted answer, from a fake provider.";

/// Says which build this is, on stderr.
pub fn announce() {
    eprintln!("{MARKER}");
}

/// What every agent does: the first words of [`ANSWER`], a wait, the rest, then its bill.
pub fn script() -> FakeScript {
    let (first, rest) = ANSWER.split_at(ANSWER.find(' ').unwrap_or(ANSWER.len()));
    FakeScript::default()
        .say(first)
        .wait(Duration::from_secs(8))
        .say(rest)
        .billed(1_200, 300)
}

/// Every catalog agent, scripted, priced like Grok 4.7 so the card has spend to show.
pub fn drivers() -> HashMap<AgentId, Arc<dyn RunDriver>> {
    Provider::ALL
        .into_iter()
        .map(|provider| {
            let driver: Arc<dyn RunDriver> = Arc::new(
                FakeDriver::new(script()).priced(Price::per_million_tokens(2_000_000, 6_000_000)),
            );
            (agent_for(provider).id, driver)
        })
        .collect()
}

/// Keys in memory, gone when the app quits.
pub fn keys() -> Keys {
    Keys(Arc::new(MemoryStore::default()))
}

/// Accepts every key, without asking its provider.
pub fn key_checker() -> KeyChecker {
    KeyChecker(Arc::new(Accepting))
}

/// The ChatGPT plan, signed out, its tokens in memory and no way to reach OpenAI.
pub fn plan() -> ChatGptPlan {
    ChatGptPlan::new(
        Arc::new(MemoryPlanStore::default()),
        Arc::new(Offline),
        Arc::new(SystemClock),
    )
}

/// The key check's stand-in: every key is fine.
struct Accepting;

impl ModelsProbe for Accepting {
    fn get<'a>(
        &'a self,
        _url: &'a str,
        _headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        Box::pin(async { Ok(200) })
    }
}

/// A transport that refuses every request: the e2e build has no network.
pub struct Offline;

impl FormTransport for Offline {
    fn post_form<'a>(
        &'a self,
        url: &'a str,
        _form: &'a Form,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move { Err(offline(url)) })
    }

    fn get<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<(u16, Vec<u8>), TransportError>> {
        Box::pin(async move { Err(offline(url)) })
    }
}

fn offline(url: &str) -> TransportError {
    TransportError::Exhausted(format!("the e2e build has no network: {url}"))
}

/// Logos: the same small picture for every provider, instead of its site.
pub struct Logos;

impl LogoFetcher for Logos {
    fn get<'a>(&'a self, _source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>> {
        Box::pin(async { Ok(png()) })
    }
}

/// A 64 × 64 PNG's signature and header: enough for the logo checks.
fn png() -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&13_u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&64_u32.to_be_bytes());
    png.extend_from_slice(&64_u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    png.extend_from_slice(&[0; 32]);
    png
}
