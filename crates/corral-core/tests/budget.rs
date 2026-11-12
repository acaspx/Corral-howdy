//! The budget meter's hard-cap half (SK-5): TP-BUD-01 and TP-BUD-05 (`docs/04-test-plan.md`,
//! system design §7, security model T9 and T25).
//!
//! Money is in micro-USD (10^-6 USD), like `usage.reported`. Prices below are $2 per million input
//! tokens and $6 per million output tokens: 2 µ$ per input token, 6 µ$ per output token.

use corral_core::budget::{
    Admission, BudgetConfig, BudgetError, MIN_OUTPUT_TOKENS, Price, RequestEstimate, RunBudget,
};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig, SupervisorError,
};
use proptest::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PRICE: Price = Price::per_million_tokens(2_000_000, 6_000_000);

fn estimate(input_tokens: u64, max_output_tokens: u32) -> RequestEstimate {
    RequestEstimate {
        price: PRICE,
        input_tokens,
        tool_fees_micro_usd: 0,
        max_output_tokens,
    }
}

fn usage(input_tokens: u64, output_tokens: u64, cost_micro_usd: u64) -> UsageReported {
    UsageReported {
        input_tokens,
        output_tokens,
        cost_micro_usd,
    }
}

// ---- TP-BUD-01: the pre-request estimate blocks a request that would cross the hard cap -------

#[test]
fn tp_bud_01_a_request_that_would_cross_the_cap_is_refused_and_reserves_nothing() {
    let mut budget = RunBudget::new(1_000);
    // 453 input tokens (906 µ$) plus the smallest useful answer (16 tokens, 96 µ$) is 1,002 µ$.
    let refused = budget.admit(&estimate(453, 16_384));
    assert_eq!(
        refused,
        Err(BudgetError::OverCap {
            cap_micro_usd: 1_000,
            committed_micro_usd: 0,
            needed_micro_usd: 1_002,
        })
    );
    assert_eq!(budget.committed(), 0, "a refused request reserves nothing");
    assert!(
        budget.admit(&estimate(10, 16_384)).is_ok(),
        "a smaller request still fits"
    );
}

#[test]
fn tp_bud_01_a_worst_case_that_lands_exactly_on_the_cap_is_allowed() {
    let mut budget = RunBudget::new(1_000);
    // 452 input tokens (904 µ$) leave 96 µ$: exactly 16 output tokens.
    let admitted = budget.admit(&estimate(452, 16_384)).expect("fits exactly");
    assert_eq!(
        admitted,
        Admission {
            max_output_tokens: 16,
            worst_case_micro_usd: 1_000,
        }
    );
    assert_eq!(budget.committed(), 1_000);
    assert!(
        budget.admit(&estimate(0, 16_384)).is_err(),
        "nothing is left for another request"
    );
}

#[test]
fn tp_bud_01_unreconciled_requests_count_at_their_worst_case() {
    let mut budget = RunBudget::new(100_000);
    let first = budget.admit(&estimate(1_000, 4_000)).expect("first");
    let second = budget.admit(&estimate(1_000, 4_000)).expect("second");
    // 2,000 µ$ of input plus 4,000 × 6 µ$ of output each: 26,000 µ$ held per request.
    assert_eq!(first.worst_case_micro_usd, 26_000);
    assert_eq!(second.worst_case_micro_usd, 26_000);
    assert_eq!(budget.committed(), 52_000);
    assert_eq!(budget.spent(), 0, "nothing is billed yet");
}

#[test]
fn tp_bud_01_reported_spend_replaces_the_reservation() {
    let mut budget = RunBudget::new(100_000);
    budget.admit(&estimate(1_000, 16_384)).expect("admitted");
    budget.reconcile(&usage(12, 47, 306));
    assert_eq!(budget.spent(), 306);
    assert_eq!(budget.committed(), 306, "the reservation is released");
    let next = budget.admit(&estimate(1_000, 16_384)).expect("room left");
    // 100,000 − 306 − 2,000 = 97,694 µ$ of output room: 16,282 tokens.
    assert_eq!(next.max_output_tokens, 16_282);
}

#[test]
fn tp_bud_01_a_missing_cost_falls_back_to_tokens_times_price() {
    // Spend is never under-counted: a provider that reports tokens but no cost (or 0) is
    // charged at the price the request was admitted at, vendor tool fees included.
    let mut budget = RunBudget::new(1_000_000);
    budget
        .admit(&RequestEstimate {
            tool_fees_micro_usd: 7,
            ..estimate(1_000, 1_000)
        })
        .expect("admitted");
    budget.reconcile(&usage(1_000, 500, 0));
    assert_eq!(budget.spent(), 2_000 + 3_000 + 7);
}

#[test]
fn tp_bud_01_a_bill_above_the_estimate_is_counted_in_full() {
    let mut budget = RunBudget::new(100_000);
    budget.admit(&estimate(1_000, 1_000)).expect("admitted");
    budget.reconcile(&usage(1_000, 1_000, 120_000));
    assert_eq!(budget.spent(), 120_000);
    assert_eq!(
        budget.admit(&estimate(0, 16)),
        Err(BudgetError::OverCap {
            cap_micro_usd: 100_000,
            committed_micro_usd: 120_000,
            needed_micro_usd: 96,
        })
    );
}

#[test]
fn tp_bud_01_usage_with_no_matching_request_still_counts() {
    let mut budget = RunBudget::new(100_000);
    budget.reconcile(&usage(10, 10, 55));
    assert_eq!(budget.spent(), 55);
}

#[test]
fn tp_bud_01_the_default_cap_is_one_dollar_per_run() {
    // Security model §"Defaults": per-run budget $1.
    assert_eq!(BudgetConfig::default().per_run_cap_micro_usd, 1_000_000);
}

// ---- TP-BUD-05: max output tokens come from the remaining budget; tool fees count first ------

#[test]
fn tp_bud_05_max_output_tokens_come_from_the_remaining_budget() {
    // $0.10 cap, 1,000 input tokens (2,000 µ$): 98,000 µ$ buys 16,333 output tokens.
    let mut tight = RunBudget::new(100_000);
    let admitted = tight.admit(&estimate(1_000, 16_384)).expect("fits");
    assert_eq!(admitted.max_output_tokens, 16_333);
    assert_eq!(admitted.worst_case_micro_usd, 2_000 + 16_333 * 6);

    // $1 cap: plenty of room, so the configured ceiling applies.
    let mut roomy = RunBudget::new(1_000_000);
    let admitted = roomy.admit(&estimate(1_000, 16_384)).expect("fits");
    assert_eq!(admitted.max_output_tokens, 16_384);
}

#[test]
fn tp_bud_05_vendor_tool_fees_are_counted_before_the_call() {
    let with_fees = |fees| RequestEstimate {
        tool_fees_micro_usd: fees,
        ..estimate(1_000, 16_384)
    };
    let mut budget = RunBudget::new(100_000);
    // 2,000 µ$ input + 50,000 µ$ fees leave 48,000 µ$: 8,000 output tokens.
    let admitted = budget.admit(&with_fees(50_000)).expect("fits");
    assert_eq!(admitted.max_output_tokens, 8_000);
    assert_eq!(admitted.worst_case_micro_usd, 100_000);

    let mut budget = RunBudget::new(100_000);
    assert_eq!(
        budget.admit(&with_fees(99_000)),
        Err(BudgetError::OverCap {
            cap_micro_usd: 100_000,
            committed_micro_usd: 0,
            needed_micro_usd: 2_000 + 99_000 + 96,
        })
    );
}

proptest! {
    /// Whatever the cap, prices, prompt size, fees and ceiling: an admitted request's worst case
    /// never goes over the cap, it gets the most output the cap allows (up to the ceiling), and a
    /// request is refused only when even the smallest useful answer wouldn't fit.
    #[test]
    fn tp_bud_05_an_admitted_worst_case_never_crosses_the_cap(
        cap in 0u64..2_000_000_000,
        already in 0u64..2_000_000_000,
        input_price in 0u64..100_000_000,
        output_price in 0u64..100_000_000,
        input_tokens in 0u64..10_000_000,
        fees in 0u64..100_000_000,
        ceiling in 0u32..300_000,
    ) {
        let price = Price::per_million_tokens(input_price, output_price);
        let mut budget = RunBudget::new(cap);
        budget.reconcile(&usage(0, 0, already));
        let remaining = cap.saturating_sub(already);
        let fixed = price.input_cost(input_tokens) + fees;
        let est = RequestEstimate {
            price,
            input_tokens,
            tool_fees_micro_usd: fees,
            max_output_tokens: ceiling,
        };
        match budget.admit(&est) {
            Ok(a) => {
                prop_assert!(a.max_output_tokens <= ceiling);
                prop_assert!(a.max_output_tokens >= MIN_OUTPUT_TOKENS.min(ceiling));
                prop_assert_eq!(
                    a.worst_case_micro_usd,
                    fixed + price.output_cost(u64::from(a.max_output_tokens))
                );
                prop_assert!(a.worst_case_micro_usd <= remaining, "over the cap");
                prop_assert!(
                    a.max_output_tokens == ceiling
                        || fixed + price.output_cost(u64::from(a.max_output_tokens) + 1) > remaining,
                    "more output would still have fit"
                );
                prop_assert_eq!(budget.committed(), already + a.worst_case_micro_usd);
            }
            Err(BudgetError::OverCap { needed_micro_usd, committed_micro_usd, cap_micro_usd }) => {
                prop_assert_eq!(cap_micro_usd, cap);
                prop_assert_eq!(committed_micro_usd, already);
                prop_assert_eq!(
                    needed_micro_usd,
                    fixed + price.output_cost(u64::from(MIN_OUTPUT_TOKENS))
                );
                prop_assert!(needed_micro_usd > remaining, "refused although it fit");
                prop_assert_eq!(budget.committed(), already, "a refusal reserves nothing");
            }
            Err(other) => prop_assert!(false, "unexpected refusal: {other}"),
        }
    }
}

// ---- Through the supervisor: the meter is the only way to log a model request ----------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn run(&self, run: &str) -> Vec<Event> {
        let id = RunId::from(run);
        let all = self.0.lock().expect("log lock");
        all.iter().filter(|e| e.run_id == id).cloned().collect()
    }

    fn kinds(&self, run: &str) -> Vec<&'static str> {
        self.run(run).iter().map(|e| e.kind().as_str()).collect()
    }

    fn assert_legal(&self, run: &str) {
        let mut state = Lifecycle::idle();
        for (i, e) in self.run(run).iter().enumerate() {
            assert_eq!(e.seq, i as u64 + 1, "seq must be gapless");
            state = next(&state, e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
    }

    fn failure(&self, run: &str) -> RunFailed {
        let last = self.run(run).pop().expect("events");
        let EventBody::RunFailed(f) = last.body else {
            panic!("expected run.failed, got {:?}", last.kind());
        };
        f
    }
}

type Script = Arc<dyn Fn(EventSink) -> RunFuture + Send + Sync>;

/// A driver that runs a test's closure with the run's sink.
struct ScriptDriver(Script);

impl RunDriver for ScriptDriver {
    fn run(&self, _request: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        (self.0)(sink)
    }
}

/// What each `request_model` call returned, in order.
type Answers = Arc<Mutex<Vec<Result<Admission, SupervisorError>>>>;

fn supervisor(cap: u64, script: Script) -> (Supervisor, Arc<MemLog>) {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig {
            per_run_cap_micro_usd: cap,
        },
        Arc::new(ScriptDriver(script)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    (sup, log)
}

fn request(run: &str) -> RunRequest {
    RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("agent"),
        session_id: SessionId::from("s1"),
        prompt: "a task".into(),
    }
}

fn model_request() -> ModelRequest {
    ModelRequest {
        provider: "test".into(),
        model: "test-model".into(),
    }
}

async fn run_all(sup: &Supervisor, runs: &[&str]) {
    for run in runs {
        sup.deploy(request(run)).expect("deploy");
    }
    tokio::time::timeout(Duration::from_secs(60), sup.wait_idle())
        .await
        .expect("runs end");
}

#[tokio::test(start_paused = true)]
async fn tp_bud_05_a_model_request_is_logged_only_through_the_meter() {
    let refused: Arc<Mutex<Vec<SupervisorError>>> = Arc::default();
    let seen = refused.clone();
    let (sup, log) = supervisor(
        1_000_000,
        Arc::new(move |sink| {
            let seen = seen.clone();
            Box::pin(async move {
                if let Err(e) = sink.emit(EventBody::ModelRequest(model_request())) {
                    seen.lock().expect("lock").push(e);
                }
                sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                    .expect("completed");
            })
        }),
    );
    run_all(&sup, &["r1"]).await;
    assert_eq!(
        *refused.lock().expect("lock"),
        [SupervisorError::Budget(BudgetError::NotAdmitted)]
    );
    assert_eq!(
        log.kinds("r1"),
        ["run.queued", "run.started", "run.completed"],
        "the bypassing model.request never reaches the log"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_a_refused_request_fails_the_run_and_logs_no_request() {
    let answers: Answers = Arc::default();
    let seen = answers.clone();
    let (sup, log) = supervisor(
        1_000,
        Arc::new(move |sink| {
            let seen = seen.clone();
            Box::pin(async move {
                let answer = sink.request_model(model_request(), &estimate(453, 16_384));
                seen.lock().expect("lock").push(answer);
            })
        }),
    );
    run_all(&sup, &["r1"]).await;
    assert_eq!(
        *answers.lock().expect("lock"),
        [Err(SupervisorError::Budget(BudgetError::OverCap {
            cap_micro_usd: 1_000,
            committed_micro_usd: 0,
            needed_micro_usd: 1_002,
        }))]
    );
    assert_eq!(log.kinds("r1"), ["run.queued", "run.started", "run.failed"]);
    log.assert_legal("r1");
    let failed = log.failure("r1");
    assert_eq!(failed.reason, FailureReason::BudgetHardCap);
    assert!(
        failed.message.contains("spend cap"),
        "the card says why: {}",
        failed.message
    );
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_the_card_names_the_cap_in_dollars() {
    let (sup, log) = supervisor(
        1_000_000,
        Arc::new(move |sink| {
            Box::pin(async move {
                // 500,000 input tokens is $1.00 before any output.
                let refused = sink.request_model(model_request(), &estimate(500_000, 16_384));
                assert!(refused.is_err());
            })
        }),
    );
    run_all(&sup, &["r1"]).await;
    let failed = log.failure("r1");
    assert!(
        failed.message.contains("$1.00 spend cap"),
        "{}",
        failed.message
    );
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_spend_carries_across_requests_in_a_run() {
    let answers: Answers = Arc::default();
    let seen = answers.clone();
    let (sup, log) = supervisor(
        100_000,
        Arc::new(move |sink| {
            let seen = seen.clone();
            Box::pin(async move {
                for cost in [60_000, 39_000] {
                    let answer = sink.request_model(model_request(), &estimate(1_000, 16_384));
                    seen.lock().expect("lock").push(answer);
                    sink.emit(EventBody::UsageReported(usage(1_000, 100, cost)))
                        .expect("usage");
                }
                // 99,000 µ$ spent: 1,000 µ$ left, less than 1,000 input tokens cost with an answer.
                let answer = sink.request_model(model_request(), &estimate(1_000, 16_384));
                seen.lock().expect("lock").push(answer);
            })
        }),
    );
    run_all(&sup, &["r1"]).await;
    let answers = answers.lock().expect("lock");
    assert_eq!(answers[0].as_ref().map(|a| a.max_output_tokens), Ok(16_333));
    // 40,000 µ$ left after the first bill, minus 2,000 µ$ of input: 6,333 output tokens.
    assert_eq!(answers[1].as_ref().map(|a| a.max_output_tokens), Ok(6_333));
    assert_eq!(
        answers[2],
        Err(SupervisorError::Budget(BudgetError::OverCap {
            cap_micro_usd: 100_000,
            committed_micro_usd: 99_000,
            needed_micro_usd: 2_096,
        }))
    );
    assert_eq!(
        log.kinds("r1"),
        [
            "run.queued",
            "run.started",
            "model.request",
            "usage.reported",
            "model.request",
            "usage.reported",
            "run.failed",
        ]
    );
    log.assert_legal("r1");
    assert_eq!(log.failure("r1").reason, FailureReason::BudgetHardCap);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_the_cap_is_per_run() {
    let answers: Answers = Arc::default();
    let seen = answers.clone();
    let (sup, log) = supervisor(
        100_000,
        Arc::new(move |sink| {
            let seen = seen.clone();
            Box::pin(async move {
                let answer = sink.request_model(model_request(), &estimate(1_000, 16_384));
                let admitted = answer.is_ok();
                seen.lock().expect("lock").push(answer);
                if admitted {
                    sink.emit(EventBody::UsageReported(usage(1_000, 100, 90_000)))
                        .expect("usage");
                    sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                        .expect("completed");
                }
            })
        }),
    );
    run_all(&sup, &["r1", "r2"]).await;
    let answers = answers.lock().expect("lock");
    assert_eq!(answers.len(), 2);
    assert!(
        answers.iter().all(Result::is_ok),
        "r1's spend doesn't count against r2: {answers:?}"
    );
    assert_eq!(log.kinds("r2").last(), Some(&"run.completed"));
}

#[tokio::test(start_paused = true)]
async fn tp_bud_01_a_supervisor_without_a_budget_config_uses_the_default_cap() {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(ScriptDriver(Arc::new(|sink| {
            Box::pin(async move {
                // $1.00 of input alone: over the default $1 cap once an answer is added.
                let refused = sink.request_model(model_request(), &estimate(500_000, 16_384));
                assert!(refused.is_err());
            })
        }))),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    run_all(&sup, &["r1"]).await;
    assert_eq!(log.failure("r1").reason, FailureReason::BudgetHardCap);
}
