//! The Anthropic Messages client (SK-7), run through the real supervisor against hand-written
//! cassettes (no network, AGENTS.md rule 7): the adapter contract suite (TP-ADP-01, 03, 04, 05),
//! no tools (TP-SKEL-02 for Claude) and the hard spend cap (TP-BUD-01, 05). Time is tokio's
//! paused clock, so backoff and Stop are exact.
//!
//! The prompt is "Say hello." (10 bytes): before sending, the input is bounded at 266 tokens,
//! 532 µ$ at $2 per million. Output is $10 per million: 10 µ$ per token.

use corral_adapters::cassette::{Cassette, CassetteTransport};
use corral_adapters::messages::{MessagesConfig, MessagesDriver};
use corral_core::budget::{BudgetConfig, Price};
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

const KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1-Wq9z"; // gitleaks:allow (made-up test key)

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/anthropic")
}

fn cassette(name: &str) -> Cassette {
    Cassette::load(&cassette_dir().join(name)).expect("cassette loads")
}

struct Harness {
    sup: Supervisor,
    log: Arc<MemLog>,
    transport: Arc<CassetteTransport>,
}

fn build(cassette: Cassette, key: Option<&str>, chunk: usize, cap: Option<u64>) -> Harness {
    let keys = MemoryStore::default();
    if let Some(key) = key {
        keys.set(Provider::Anthropic, &Secret::new(key).expect("valid key"))
            .expect("store");
    }
    let transport = Arc::new(CassetteTransport::new(cassette, chunk));
    let driver = MessagesDriver::new(
        transport.clone(),
        Arc::new(keys),
        MessagesConfig::anthropic("claude-test"),
    );
    let log = Arc::new(MemLog::default());
    let budget = cap.map_or_else(BudgetConfig::default, |cap| BudgetConfig {
        per_run_cap_micro_usd: cap,
    });
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        budget,
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

fn harness_with(cassette: Cassette, key: Option<&str>, chunk: usize) -> Harness {
    build(cassette, key, chunk, None)
}

fn harness(name: &str) -> Harness {
    harness_with(cassette(name), Some(KEY), 4096)
}

fn request() -> RunRequest {
    RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("claude"),
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

fn response(h: &Harness) -> ModelResponse {
    h.log
        .events()
        .into_iter()
        .find_map(|e| {
            let EventBody::ModelResponse(r) = e.body else {
                return None;
            };
            Some(r)
        })
        .expect("model.response")
}

fn usage(h: &Harness) -> UsageReported {
    h.log
        .events()
        .into_iter()
        .find_map(|e| {
            let EventBody::UsageReported(u) = e.body else {
                return None;
            };
            Some(u)
        })
        .expect("usage.reported")
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
async fn tp_adp_01_a_claude_answer_streams_in_order_with_one_terminal_event() {
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
        assert_eq!(
            deltas,
            ["Hello", " from", " Claude."],
            "thinking isn't shown as the answer"
        );
        let r = response(&h);
        assert_eq!(r.text, "Hello from Claude.");
        assert_eq!(r.finish_reason.as_deref(), Some("end_turn"));
        let EventBody::ModelRequest(r) = &events[2].body else {
            panic!("expected model.request, got {:?}", events[2].kind());
        };
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.model, "claude-test");
        assert_key_never_logged(&h);
    }
}

#[tokio::test(start_paused = true)]
async fn tp_adp_01_hitting_max_tokens_still_completes() {
    let h = harness("max-tokens.json");
    run_to_end(&h).await;
    assert_eq!(h.log.terminal().kind().as_str(), "run.completed");
    let r = response(&h);
    assert_eq!(r.text, "Hello");
    assert_eq!(r.finish_reason.as_deref(), Some("max_tokens"));
}

// ---- TP-ADP-05: usage --------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_05_usage_is_reported_and_priced_from_tokens() {
    let h = harness("text-ok.json");
    run_to_end(&h).await;
    let u = usage(&h);
    assert_eq!(u.input_tokens, 12);
    assert_eq!(u.output_tokens, 47, "output includes thinking tokens");
    // Anthropic reports no cost: 12 × 2 µ$ + 47 × 10 µ$.
    assert_eq!(u.cost_micro_usd, 494);
}

#[tokio::test(start_paused = true)]
async fn tp_adp_05_cached_input_counts_and_output_is_cumulative() {
    let h = harness("cached-input.json");
    run_to_end(&h).await;
    let u = usage(&h);
    // Fresh, cache-write and cache-read input are all input; every one is billed.
    assert_eq!(u.input_tokens, 18);
    // message_delta's output_tokens is a running total, not an increment.
    assert_eq!(u.output_tokens, 30);
    // Priced at the full input rate, so spend is never under-counted.
    assert_eq!(u.cost_micro_usd, 18 * 2 + 30 * 10);
}

// ---- TP-SKEL-02 for Claude: no tools; the request Anthropic expects -------------------------

#[tokio::test(start_paused = true)]
async fn tp_skel_02_requests_carry_no_tools_and_the_key_goes_in_x_api_key() {
    let h = harness("text-ok.json");
    run_to_end(&h).await;
    let seen = h.transport.seen();
    assert_eq!(seen.len(), 1);
    let req = &seen[0];
    assert_eq!(req.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(
        req.bearer_last4, "Wq9z",
        "the key from the store, read at request time"
    );
    assert_eq!(req.key_header, "x-api-key");
    assert!(
        req.headers
            .iter()
            .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01"),
        "{:?}",
        req.headers
    );
    let body = req.body.as_object().expect("a JSON object");
    let mut keys: Vec<&str> = body.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["max_tokens", "messages", "model", "stream"],
        "no tools, tool_choice, mcp_servers or system prompt"
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "claude-test");
    assert_eq!(
        body["messages"],
        serde_json::json!([{"role": "user", "content": "Say hello."}])
    );
    assert_eq!(body["max_tokens"], 16_384, "the ceiling, with $1 of room");
}

#[tokio::test(start_paused = true)]
async fn tp_skel_02_a_tool_block_fails_the_run_and_nothing_runs() {
    for (name, tool) in [
        ("tool-call.json", "send_email"),
        ("server-tool-call.json", "web_search"),
    ] {
        let h = harness(name);
        run_to_end(&h).await;
        let f = failure(&h);
        assert_eq!(f.reason, FailureReason::ToolsNotEnabled, "{name}");
        assert!(
            f.message.contains(tool),
            "{name} names the tool: {}",
            f.message
        );
        let kinds = h.log.kinds();
        assert!(!kinds.iter().any(|k| k.starts_with("tool.")), "{kinds:?}");
        assert!(!kinds.contains(&"model.response"), "{name}");
    }
}

// ---- TP-SKEL-01 (driver half): no key, nothing sent ---------------------------------------------

#[tokio::test(start_paused = true)]
async fn without_an_anthropic_key_the_run_fails_and_nothing_is_sent() {
    let h = harness_with(cassette("text-ok.json"), None, 4096);
    run_to_end(&h).await;
    assert_eq!(h.log.kinds(), ["run.queued", "run.started", "run.failed"]);
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::Provider);
    assert_eq!(f.message, "Add your Anthropic key");
    assert!(h.transport.seen().is_empty());
}

// ---- TP-ADP-04: provider errors are typed ------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_adp_04_a_rejected_key_says_so_and_isnt_retried() {
    let h = harness("unauthorized.json");
    run_to_end(&h).await;
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::Provider);
    assert!(
        f.message.contains("Anthropic rejected the key"),
        "{}",
        f.message
    );
    assert_eq!(h.transport.seen().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn tp_adp_04_rate_limits_and_overload_are_retried() {
    for (name, at_least) in [
        ("rate-limited-then-ok.json", Duration::from_secs(2)),
        // 529 has no Retry-After: the first backoff is 1 s.
        ("overloaded-then-ok.json", Duration::from_secs(1)),
    ] {
        let h = harness(name);
        let start = Instant::now();
        run_to_end(&h).await;
        assert_eq!(h.log.terminal().kind().as_str(), "run.completed", "{name}");
        assert_eq!(h.transport.seen().len(), 2, "{name}");
        assert!(start.elapsed() >= at_least, "{name} waited before retrying");
        // model.request is logged once per run, not per attempt.
        assert_eq!(h.log.kinds(), ANSWER_KINDS, "{name}");
    }
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
async fn tp_adp_04_server_errors_and_stream_errors_are_provider_failures() {
    for (name, needle) in [
        ("server-error.json", "HTTP 500: Internal server error"),
        ("stream-error.json", "Overloaded"),
    ] {
        let h = harness(name);
        run_to_end(&h).await;
        let f = failure(&h);
        assert_eq!(f.reason, FailureReason::Provider, "{name}");
        assert!(f.message.contains(needle), "{name}: {}", f.message);
        assert!(!h.log.kinds().contains(&"model.response"), "{name}");
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
async fn tp_adp_03_stop_ends_a_streaming_claude_run_promptly() {
    let h = harness("stall.json");
    h.sup.deploy(request()).expect("deploy");
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

// ---- TP-BUD-01, 05: the hard spend cap -----------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_bud_05_max_tokens_comes_from_the_remaining_budget() {
    // 1,532 µ$: 532 for the bounded input leaves 1,000, which buys 100 output tokens.
    let h = build(cassette("text-ok.json"), Some(KEY), 4096, Some(1_532));
    run_to_end(&h).await;
    let seen = h.transport.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["max_tokens"], 100);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_over_the_cap_nothing_is_sent_to_anthropic() {
    // 600 µ$: after 532 for input, 68 can't buy the 16-token minimum answer (160 µ$).
    let h = build(cassette("text-ok.json"), Some(KEY), 4096, Some(600));
    run_to_end(&h).await;
    assert!(h.transport.seen().is_empty(), "nothing may be sent");
    let f = failure(&h);
    assert_eq!(f.reason, FailureReason::BudgetHardCap);
}

#[test]
fn the_default_config_is_claude_sonnet_5_5_at_list_price() {
    let config = MessagesConfig::anthropic("claude-sonnet-5-5");
    assert_eq!(config.provider, Provider::Anthropic);
    assert_eq!(config.url(), "https://api.anthropic.com/v1/messages");
    assert_eq!(
        config.price,
        Price::per_million_tokens(2_000_000, 10_000_000)
    );
    assert_eq!(config.max_attempts, 3);
}
