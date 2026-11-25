//! Long-prompt prices in the clients (TP-BUD-08; security model T25): Grok's configuration
//! carries xAI's long-prompt rate, and Claude's reported cost, which Corral computes from tokens
//! because Anthropic reports none, uses whichever rate the request's prompt pays.
//!
//! The cassette's run reports 12 input and 47 output tokens.

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_adapters::responses::ResponsesConfig;
use corral_core::budget::Price;
use corral_core::clock::ManualClock;
use corral_core::event::{Event, EventBody, UsageReported};
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "sk-ant-api03-Lp4mT8vQ2nR6sW0xY3zA7bC1dE5fG9hJ-Kd2r"; // gitleaks:allow (made-up test key)

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

/// The usage Claude reports for the cassette's run, priced at `price`.
async fn claude_usage(price: Price) -> UsageReported {
    let keys = MemoryStore::default();
    keys.set(Provider::Anthropic, &Secret::new(KEY).expect("valid key"))
        .expect("store");
    let cassette = Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/anthropic/text-ok.json"),
    )
    .expect("cassette loads");
    let driver = MessagesDriver::new(
        Arc::new(CassetteTransport::new(cassette, 4096)),
        Arc::new(keys),
        MessagesConfig {
            price,
            ..MessagesConfig::anthropic("claude-test")
        },
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
        agent_id: AgentId::from("claude"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("the run ends");
    let events = log.0.lock().expect("log lock");
    events
        .iter()
        .find_map(|e| {
            let EventBody::UsageReported(u) = &e.body else {
                return None;
            };
            Some(u.clone())
        })
        .expect("usage.reported")
}

#[tokio::test(start_paused = true)]
async fn tp_bud_08_claudes_cost_uses_the_rate_its_prompt_pays() {
    let base = Price::per_million_tokens(2_000_000, 10_000_000);
    let long = claude_usage(base.with_long_prompt(12, 4_000_000, 20_000_000)).await;
    assert_eq!(
        long.cost_micro_usd,
        12 * 4 + 47 * 20,
        "12 tokens reach the threshold"
    );
    let short = claude_usage(base.with_long_prompt(13, 4_000_000, 20_000_000)).await;
    assert_eq!(
        short.cost_micro_usd,
        12 * 2 + 47 * 10,
        "12 tokens fall short of it"
    );
}

#[test]
fn tp_bud_08_grok_doubles_from_200_000_prompt_tokens() {
    assert_eq!(
        ResponsesConfig::xai("grok-4.7").price,
        Price::per_million_tokens(2_000_000, 6_000_000)
            .with_long_prompt(200_000, 4_000_000, 12_000_000)
    );
}
