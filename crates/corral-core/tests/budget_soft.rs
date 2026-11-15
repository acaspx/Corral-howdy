//! The budget meter's soft limit (M2 item 5, TP-BUD-02, system design §7): below it, a request's
//! worst case is held under it; at it, the next request is refused until the user approves going
//! on, and `budget.warned` is logged once. Approving lifts the limit to the hard cap for the rest
//! of the run. The run isn't failed by the soft limit: the driver asks (a `budget.threshold`
//! checkpoint) and either continues or stops.
//!
//! Prices are $2 per million input tokens and $6 per million output tokens: 2 µ$ per input
//! token, 6 µ$ per output token.

use corral_core::budget::{BudgetError, Price, RequestEstimate, RunBudget};
use corral_core::clock::ManualClock;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, Phase, next};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig, SupervisorError,
};
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

fn usage(cost_micro_usd: u64) -> UsageReported {
    UsageReported {
        input_tokens: 0,
        output_tokens: 0,
        cost_micro_usd,
    }
}

// ---- The meter ---------------------------------------------------------------------------------

#[test]
fn tp_bud_02_below_the_soft_limit_a_request_is_held_under_it() {
    let mut budget = RunBudget::new(1_000_000).with_soft_limit(500_000);
    // 1,000 input tokens are 2,000 µ$; the 498,000 µ$ left under the soft limit buys 83,000
    // output tokens.
    let admitted = budget.admit(&estimate(1_000, 1_000_000)).expect("admitted");
    assert_eq!(admitted.max_output_tokens, 83_000);
    assert_eq!(
        admitted.worst_case_micro_usd, 500_000,
        "lands exactly on the soft limit"
    );
    assert_eq!(budget.soft_limit(), Some(500_000));
}

#[test]
fn tp_bud_02_at_the_soft_limit_a_request_waits_for_approval_then_the_cap_applies() {
    let mut budget = RunBudget::new(1_000_000).with_soft_limit(500_000);
    budget.admit(&estimate(1_000, 1_000)).expect("first");
    budget.reconcile(&usage(500_000));
    assert_eq!(
        budget.admit(&estimate(1_000, 1_000)),
        Err(BudgetError::SoftLimit {
            limit_micro_usd: 500_000,
            committed_micro_usd: 500_000,
            cap_micro_usd: 1_000_000,
        })
    );
    assert_eq!(budget.committed(), 500_000, "a refusal holds nothing");
    assert!(!budget.past_soft_limit());
    budget.approve_soft_limit();
    assert!(budget.past_soft_limit());
    let admitted = budget.admit(&estimate(1_000, 1_000_000)).expect("approved");
    // 498,000 µ$ left under the cap now.
    assert_eq!(admitted.max_output_tokens, 83_000);
    budget.reconcile(&usage(500_000));
    assert!(
        matches!(
            budget.admit(&estimate(1_000, 1_000)),
            Err(BudgetError::OverCap { .. })
        ),
        "the hard cap still holds"
    );
}

#[test]
fn tp_bud_02_a_request_that_would_cross_the_soft_limit_partway_is_refused_too() {
    let mut budget = RunBudget::new(1_000_000).with_soft_limit(500_000);
    budget.reconcile(&usage(499_000));
    // 453 input tokens (906 µ$) plus 16 output tokens (96 µ$) need 1,002 µ$; 1,000 µ$ are left.
    assert!(matches!(
        budget.admit(&estimate(453, 1_000)),
        Err(BudgetError::SoftLimit { .. })
    ));
    assert!(
        budget.admit(&estimate(452, 1_000)).is_ok(),
        "1,000 µ$ exactly fits"
    );
}

#[test]
fn tp_bud_02_a_soft_limit_at_or_over_the_cap_is_no_soft_limit() {
    for soft in [1_000_000, 2_000_000] {
        let mut budget = RunBudget::new(1_000_000).with_soft_limit(soft);
        assert_eq!(budget.soft_limit(), None, "{soft}");
        budget.reconcile(&usage(1_000_000));
        assert!(matches!(
            budget.admit(&estimate(1, 16)),
            Err(BudgetError::OverCap { .. })
        ));
    }
    assert_eq!(
        RunBudget::new(1_000_000).soft_limit(),
        None,
        "none by default"
    );
}

#[test]
fn tp_bud_02_the_soft_limit_reads_plainly() {
    let refusal = BudgetError::SoftLimit {
        limit_micro_usd: 500_000,
        committed_micro_usd: 512_300,
        cap_micro_usd: 1_000_000,
    };
    assert_eq!(
        refusal.to_string(),
        "This run reached its $0.50 soft limit ($0.5123 used so far). Approve to let it go on, up to its $1.00 cap."
    );
}

// ---- Through the supervisor --------------------------------------------------------------------

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

    fn state(&self) -> Lifecycle {
        let mut state = Lifecycle::idle();
        for e in self.events() {
            state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
        }
        state
    }
}

type Script = Arc<dyn Fn(EventSink) -> RunFuture + Send + Sync>;

struct ScriptDriver(Script);

impl RunDriver for ScriptDriver {
    fn run(&self, _request: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        (self.0)(sink)
    }
}

fn model_request() -> ModelRequest {
    ModelRequest {
        provider: "test".into(),
        model: "test-model".into(),
    }
}

/// What the script saw, step by step.
type Seen = Arc<Mutex<Vec<String>>>;

fn describe(answer: &Result<corral_core::budget::Admission, SupervisorError>) -> String {
    match answer {
        Ok(_) => "admitted".into(),
        Err(SupervisorError::Budget(BudgetError::SoftLimit { .. })) => "soft limit".into(),
        Err(SupervisorError::Budget(BudgetError::OverCap { .. })) => "over cap".into(),
        Err(other) => format!("other: {other}"),
    }
}

async fn run(soft: Option<u64>, script: Script) -> Arc<MemLog> {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(ScriptDriver(script)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    sup.set_soft_limit(soft);
    sup.deploy(RunRequest {
        run_id: RunId::from("r1"),
        agent_id: AgentId::from("agent"),
        session_id: SessionId::from("s1"),
        prompt: "a task".into(),
    })
    .expect("deploy");
    tokio::time::timeout(Duration::from_secs(60), sup.wait_idle())
        .await
        .expect("the run ends");
    log
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_the_sink_refuses_at_the_soft_limit_warns_once_and_leaves_the_run_going() {
    let seen: Seen = Arc::default();
    let s = Arc::clone(&seen);
    let script: Script = Arc::new(move |sink: EventSink| {
        let seen = Arc::clone(&s);
        Box::pin(async move {
            let note = |text: String| seen.lock().expect("seen").push(text);
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            sink.emit(EventBody::UsageReported(usage(600_000)))
                .expect("usage");
            // At the soft limit, twice: refused both times, warned once.
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            note(format!("phase {:?}", sink.phase().expect("phase")));
            sink.raise_to_cap().expect("approved");
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("done");
        })
    });
    // The default cap is $1; the soft limit is $0.50.
    let log = run(Some(500_000), script).await;
    assert_eq!(
        *seen.lock().expect("seen"),
        [
            "admitted",
            "soft limit",
            "soft limit",
            "phase Running",
            "admitted"
        ]
    );
    let warned: Vec<BudgetWarned> = log
        .events()
        .into_iter()
        .filter_map(|e| {
            let EventBody::BudgetWarned(w) = e.body else {
                return None;
            };
            Some(w)
        })
        .collect();
    assert_eq!(
        warned,
        [BudgetWarned {
            scope: BudgetScope::Run,
            spent_micro_usd: 600_000,
            limit_micro_usd: 500_000,
        }],
        "warned once per threshold"
    );
    assert_eq!(
        log.kinds()
            .iter()
            .filter(|k| **k == "model.request")
            .count(),
        2,
        "the refused requests weren't logged"
    );
    assert_eq!(log.state().phase(), Phase::Completed);
}

#[tokio::test(start_paused = true)]
async fn tp_bud_02_without_a_soft_limit_nothing_changes() {
    let seen: Seen = Arc::default();
    let s = Arc::clone(&seen);
    let script: Script = Arc::new(move |sink: EventSink| {
        let seen = Arc::clone(&s);
        Box::pin(async move {
            let note = |text: String| seen.lock().expect("seen").push(text);
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            sink.emit(EventBody::UsageReported(usage(900_000)))
                .expect("usage");
            note(describe(
                &sink.request_model(model_request(), &estimate(10, 100)),
            ));
            note(format!("raise: {}", sink.raise_to_cap().is_ok()));
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("done");
        })
    });
    let log = run(None, script).await;
    assert_eq!(
        *seen.lock().expect("seen"),
        ["admitted", "admitted", "raise: true"]
    );
    assert!(!log.kinds().contains(&"budget.warned"));
}
