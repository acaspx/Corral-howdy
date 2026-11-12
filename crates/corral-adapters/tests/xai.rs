//! The xAI Responses client, run through the real supervisor against cassettes (no network,
//! AGENTS.md rule 7): TP-ADP-01, 03, 04, 05, TP-SKEL-02 and the driver half of TP-SKEL-01
//! (`docs/04-test-plan.md`, SK-4). Time is tokio's paused clock, so backoff and Stop are exact.

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::responses::{ResponsesConfig, ResponsesDriver};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{EventLog, LogError, RunRequest, Supervisor, SupervisorConfig};
use corral_secrets::{MemoryStore, Provider, Secret, SecretStore};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

// ---- Harness ----------------------------------------------------------------------------------

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

    fn terminal(&self) -> Event {
        self.events().last().cloned().expect("at least one event")
    }

    fn assert_legal(&self) {
        let mut state = Lifecycle::idle();
        for (i, e) in self.events().iter().enumerate() {
            assert_eq!(e.seq, i as u64 + 1, "seq must be gapless");
            state = next(&state, e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }
}

fn cassette_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/xai")
}

fn cassette(name: &str) -> Cassette {
    Cassette::load(&cassette_dir().join(name)).expect("cassette loads")
}

struct Harness {
    sup: Supervisor,
    log: Arc<MemLog>,
    transport: Arc<CassetteTransport>,
}

fn harness_with(cassette: Cassette, key: Option<&str>, chunk: usize) -> Harness {
    let keys = MemoryStore::default();
    if let Some(key) = key {
        keys.set(Provider::Xai, &Secret::new(key).expect("valid key"))
            .expect("store");
    }
    let transport = Arc::new(CassetteTransport::new(cassette, chunk));
    let driver = ResponsesDriver::new(
        transport.clone(),
        Arc::new(keys),
        ResponsesConfig::xai("grok-test"),
    );
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(driver),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    Harness {
        sup,
        log,
        transport,
    }
}

fn harness(name: &str) -> Harness {
    harness_with(cassette(name), Some(KEY), 4096)
}

fn request() -> RunRequest {
    RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("grok"),
        session_id: SessionId::from("s1"),
        prompt: "Say hello.".into(),
    }
}

async fn run_to_end(h: &Harness) {
    h.sup.deploy(request()).expect("deploy");
    tokio::time::timeout(Duration::from_secs(600), h.sup.wait_idle())
        .await
        .expect("the run must end");
    h.log.assert_legal();
}

fn failure(h: &Harness) -> RunFailed {
    let terminal = h.log.terminal();
    let EventBody::RunFailed(f) = terminal.body else {
        panic!("expected run.failed, got {:?}", terminal.kind());
    };
    f
}

/// Every 5-character slice of the key: none may appear in any logged event.
fn assert_key_never_logged(h: &Harness) {
    let all = serde_json::to_string(&h.log.events()).expect("events serialize");
    for i in 0..=KEY.len() - 5 {
        assert!(
            !all.contains(&KEY[i..i + 5]),
            "an event contains part of the key"
        );
    }
}

const ANSWER_KINDS: [&str; 9] = [
    "run.queued",
    "run.started",
    "model.request",
    "model.delta",
    "model.delta",
    "model.delta",
    "model.response",
    "usage.reported",
    "run.completed",
];

// ---- TP-ADP-01: event order, exactly one terminal event --------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_01_a_text_answer_streams_in_order_with_one_terminal_event() {
    // Chunk sizes from 1 byte up: the SSE parser must not care where the network splits.
    for chunk in [1, 7, 64, 4096] {
        let h = harness_with(cassette("text-ok.json"), Some(KEY), chunk);
        run_to_end(&h).await;
        assert_eq!(h.log.kinds(), ANSWER_KINDS, "chunk size {chunk}");
        let events = h.log.events();
        let deltas: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let EventBody::ModelDelta(d) = &e.body else {
                    return None;
                };
                Some(d.text.clone())
            })
            .collect();
        assert_eq!(deltas, ["Hello", " from", " Grok."]);
        let EventBody::ModelResponse(r) = &events[6].body else {
            panic!("expected model.response, got {:?}", events[6].kind());
        };
        assert_eq!(r.text, "Hello from Grok.");
        assert_eq!(r.finish_reason.as_deref(), Some("completed"));
        let EventBody::ModelRequest(r) = &events[2].body else {
            panic!("expected model.request, got {:?}", events[2].kind());
        };
        assert_eq!(r.provider, "xai");
        assert_eq!(r.model, "grok-test");
        assert_key_never_logged(&h);
    }
}

#[tokio::test(start_paused = true)]
async fn tp_adp_01_hitting_max_output_tokens_still_completes() {
    let h = harness("incomplete.json");
    run_to_end(&h).await;
    assert_eq!(h.log.terminal().kind().as_str(), "run.completed");
    let response = h.log.events().into_iter().find_map(|e| {
        let EventBody::ModelResponse(r) = e.body else {
            return None;
        };
        Some(r)
    });
    let response = response.expect("model.response");
    assert_eq!(response.text, "Hello");
    assert_eq!(response.finish_reason.as_deref(), Some("max_output_tokens"));
}

// ---- TP-ADP-05: usage --------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_05_usage_is_reported_with_the_billed_cost() {
    let h = harness("text-ok.json");
    run_to_end(&h).await;
    let usage = h.log.events().into_iter().find_map(|e| {
        let EventBody::UsageReported(u) = e.body else {
            return None;
        };
        Some(u)
    });
    let usage = usage.expect("usage.reported");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 47, "output includes reasoning tokens");
    // 3,060,000 ticks (10^-10 USD) = 306 micro-USD, rounded up so spend is never under-counted.
    assert_eq!(usage.cost_micro_usd, 306);
}

// ---- TP-SKEL-02: no tools ----------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_skel_02_requests_carry_no_tools_and_store_nothing_at_xai() {
    let h = harness("text-ok.json");
    run_to_end(&h).await;
    let seen = h.transport.seen();
    assert_eq!(seen.len(), 1);
    let req = &seen[0];
    assert_eq!(req.url, "https://api.x.ai/v1/responses");
    assert_eq!(
        req.bearer_last4, "hJk1",
        "the key from the store, read at request time"
    );
    let body = req.body.as_object().expect("a JSON object");
    let mut keys: Vec<&str> = body.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["input", "max_output_tokens", "model", "store", "stream"],
        "no tools, tool_choice or provider-side tools"
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["model"], "grok-test");
    assert_eq!(body["input"], "Say hello.");
}

#[tokio::test(start_paused = true)]
async fn tp_skel_02_a_tool_call_fails_the_run_and_nothing_runs() {
    let h = harness("tool-call.json");
    run_to_end(&h).await;
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::ToolsNotEnabled);
    assert!(
        f.message.contains("send_email"),
        "names the tool: {}",
        f.message
    );
    let kinds = h.log.kinds();
    assert!(!kinds.iter().any(|k| k.starts_with("tool.")), "{kinds:?}");
    assert!(!kinds.contains(&"model.response"));
}

// ---- TP-SKEL-01 (driver half): no key, nothing sent ---------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_skel_01_without_a_key_the_run_fails_and_nothing_is_sent() {
    let h = harness_with(cassette("text-ok.json"), None, 4096);
    run_to_end(&h).await;
    assert_eq!(h.log.kinds(), ["run.queued", "run.started", "run.failed"]);
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::Provider);
    assert_eq!(f.message, "Add your xAI key");
    assert!(
        h.transport.seen().is_empty(),
        "nothing may be sent without a key"
    );
}

// ---- TP-ADP-04: provider errors are typed ------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_04_a_rejected_key_says_so_without_echoing_xai() {
    let h = harness("unauthorized.json");
    run_to_end(&h).await;
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::Provider);
    assert!(f.message.contains("rejected the key"), "{}", f.message);
    assert!(
        !f.message.contains("xa***"),
        "xAI's masked key isn't repeated"
    );
    assert_eq!(h.transport.seen().len(), 1, "a rejected key isn't retried");
}

#[tokio::test(start_paused = true)]
async fn tp_adp_04_rate_limits_are_retried_after_retry_after() {
    let h = harness("rate-limited-then-ok.json");
    let start = Instant::now();
    run_to_end(&h).await;
    assert_eq!(h.log.terminal().kind().as_str(), "run.completed");
    assert_eq!(h.transport.seen().len(), 2);
    assert!(
        start.elapsed() >= Duration::from_secs(2),
        "waited for Retry-After"
    );
    // model.request is logged once per run, not per attempt.
    assert_eq!(h.log.kinds(), ANSWER_KINDS);
}

#[tokio::test(start_paused = true)]
async fn tp_adp_04_a_persistent_rate_limit_fails_after_three_attempts() {
    let h = harness("rate-limited.json");
    run_to_end(&h).await;
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::Provider);
    assert!(f.message.contains("rate limit"), "{}", f.message);
    assert_eq!(h.transport.seen().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn tp_adp_04_server_errors_and_failed_responses_are_provider_failures() {
    for (name, needle) in [
        ("server-error.json", "500"),
        ("response-failed.json", "The model run failed."),
    ] {
        let h = harness(name);
        run_to_end(&h).await;
        let f = failure(&h);
        assert_eq!(f.reason, FailureReason::Provider, "{name}");
        assert!(f.message.contains(needle), "{name}: {}", f.message);
    }
}

#[tokio::test(start_paused = true)]
async fn tp_adp_04_a_broken_stream_is_an_adapter_failure() {
    for name in ["malformed.json", "truncated.json"] {
        let h = harness(name);
        run_to_end(&h).await;
        let f = failure(&h);
        assert_eq!(f.reason, FailureReason::Adapter, "{name}: {}", f.message);
        assert_key_never_logged(&h);
    }
}

// ---- TP-ADP-03: Stop -----------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_03_stop_ends_a_streaming_run_promptly() {
    let h = harness("stall.json");
    h.sup.deploy(request()).expect("deploy");
    // Let it stream until the first delta.
    for _ in 0..1_000 {
        if h.log.kinds().contains(&"model.delta") {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        h.log.kinds().contains(&"model.delta"),
        "the stall cassette streams some text first"
    );
    let start = Instant::now();
    h.sup.cancel(&RunId::from("r1")).await.expect("cancel");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the driver stopped on its own, before the 2 s force-drop"
    );
    h.sup.wait_idle().await;
    h.log.assert_legal();
    assert_eq!(h.log.terminal().kind().as_str(), "run.cancelled");
}

// ---- Recorded cassettes (Option B): the same decoder must accept what xAI really sends --------

#[tokio::test(start_paused = true)]
async fn recorded_xai_cassettes_replay_cleanly() {
    let dir = cassette_dir().join("recorded");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "no recorded cassettes yet ({}); run the recorder on a Mac",
            dir.display()
        );
        return;
    };
    let mut checked = 0;
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let h = harness_with(Cassette::load(&path).expect("loads"), Some(KEY), 64);
        run_to_end(&h).await;
        if name.starts_with("text") {
            assert_eq!(h.log.terminal().kind().as_str(), "run.completed", "{name}");
            let kinds = h.log.kinds();
            assert!(kinds.contains(&"model.delta"), "{name}: {kinds:?}");
            assert!(kinds.contains(&"usage.reported"), "{name}: {kinds:?}");
        } else if name.starts_with("unauthorized") {
            assert_eq!(failure(&h).reason, FailureReason::Provider, "{name}");
        } else {
            panic!("unexpected recorded cassette {name}");
        }
        checked += 1;
    }
    eprintln!("replayed {checked} recorded cassettes");
}
