//! The xAI client under the hard spend cap (SK-5): TP-BUD-01 and TP-BUD-05 end to end, against
//! cassettes (no network, AGENTS.md rule 7).
//!
//! The prompt is "Say hello." (10 bytes). Before sending, the input is bounded at one token per
//! byte plus `REQUEST_OVERHEAD_TOKENS` (256): 266 tokens, 532 µ$ at grok-4.7's $2 per million.
//! Output is $6 per million: 6 µ$ per token.

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_core::budget::{BudgetConfig, Price, REQUEST_OVERHEAD_TOKENS};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

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

    fn kinds(&self) -> Vec<&'static str> {
        self.events().iter().map(|e| e.kind().as_str()).collect()
    }
}

struct Harness {
    log: Arc<MemLog>,
    transport: Arc<CassetteTransport>,
}

async fn run_with_cap(cap_micro_usd: u64) -> Harness {
    let keys = MemoryStore::default();
    keys.set(Provider::Xai, &Secret::new(KEY).expect("valid key"))
        .expect("store");
    let cassette = Cassette::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/xai/text-ok.json"),
    )
    .expect("cassette loads");
    let transport = Arc::new(CassetteTransport::new(cassette, 4096));
    let driver = ResponsesDriver::new(
        transport.clone(),
        Arc::new(keys),
        ResponsesConfig::xai("grok-test"),
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: cap_micro_usd,
        },
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("grok"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), sup.wait_idle())
        .await
        .expect("the run must end");
    let mut state = Lifecycle::idle();
    for e in log.events() {
        state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
    }
    Harness { log, transport }
}

#[test]
fn the_xai_config_prices_requests_at_grok_4_7_list_price() {
    assert_eq!(
        ResponsesConfig::xai("grok-4.7").price,
        Price::per_million_tokens(2_000_000, 6_000_000)
    );
    assert_eq!(REQUEST_OVERHEAD_TOKENS, 256);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_a_prompt_that_would_cross_the_cap_sends_nothing() {
    // 532 µ$ of input alone is over a 500 µ$ cap.
    let h = run_with_cap(500).await;
    assert!(
        h.transport.seen().is_empty(),
        "nothing may be sent over the cap"
    );
    assert_eq!(h.log.kinds(), ["run.queued", "run.started", "run.failed"]);
    let last = h.log.events().pop().expect("events");
    let EventBody::RunFailed(failed) = last.body else {
        panic!("expected run.failed, got {:?}", last.kind());
    };
    assert_eq!(failed.reason, FailureReason::BudgetHardCap);
    assert!(failed.message.contains("spend cap"), "{}", failed.message);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_05_max_output_tokens_come_from_the_remaining_budget() {
    // A 1¢ cap: 10,000 − 532 = 9,468 µ$ of output room, 1,578 tokens at 6 µ$ each.
    let h = run_with_cap(10_000).await;
    let seen = h.transport.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["max_output_tokens"], 1_578);
    assert_eq!(h.log.kinds().last(), Some(&"run.completed"));
}

#[tokio::test(start_paused = true)]
async fn tp_bud_05_with_room_to_spare_the_configured_ceiling_applies() {
    let h = run_with_cap(1_000_000).await;
    let seen = h.transport.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["max_output_tokens"], 16_384);
}
