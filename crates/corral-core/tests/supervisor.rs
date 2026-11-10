//! Supervisor: TP-SUP-01 and TP-SUP-02 (`docs/04-test-plan.md`, system design §8).
//!
//! Time is tokio's paused clock, so the 2-second force-drop is checked exactly without waiting.

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, Phase, next};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

// ---- Test doubles ----------------------------------------------------------------------------

/// An in-memory event log.
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

    /// Every run's events follow the lifecycle from Idle, with seq 1, 2, 3…
    fn assert_every_run_is_legal(&self) {
        let all = self.0.lock().expect("log lock").clone();
        let mut runs: HashMap<RunId, Vec<Event>> = HashMap::new();
        for e in all {
            runs.entry(e.run_id.clone()).or_default().push(e);
        }
        for (run, events) in runs {
            let mut state = Lifecycle::idle();
            for (i, e) in events.iter().enumerate() {
                assert_eq!(e.seq, i as u64 + 1, "{run}: seq must be gapless");
                state = next(&state, e)
                    .unwrap_or_else(|err| panic!("{run}: illegal event in the log: {err}"));
            }
        }
    }
}

/// How the scripted driver behaves for one run.
#[derive(Clone)]
enum Behavior {
    /// Streams a little text and completes.
    Complete,
    /// Waits until the test releases it, then completes.
    WaitForRelease(Arc<Notify>),
    /// Stops as soon as it's cancelled.
    ObeyCancel,
    /// Ignores cancel and would run forever: must be force-dropped.
    IgnoreCancel,
    /// Returns without a terminal event.
    ReturnEarly,
    /// Tries an illegal event, then completes.
    TryIllegal,
}

#[derive(Default)]
struct Script {
    behaviors: Mutex<HashMap<String, Behavior>>,
    started: Mutex<Vec<String>>,
    dropped: Arc<AtomicUsize>,
    illegal_refused: AtomicUsize,
}

/// Counts drivers whose futures were dropped (force-drop).
struct DropFlag(Arc<AtomicUsize>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ScriptDriver(Arc<Script>);

impl RunDriver for ScriptDriver {
    fn run(&self, req: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let script = self.0.clone();
        let behavior = script
            .behaviors
            .lock()
            .expect("behaviors")
            .get(req.run_id.as_str())
            .cloned()
            .unwrap_or(Behavior::Complete);
        script
            .started
            .lock()
            .expect("started")
            .push(req.run_id.as_str().to_owned());
        Box::pin(async move {
            let _guard = DropFlag(script.dropped.clone());
            let delta = || {
                EventBody::ModelDelta(ModelDelta {
                    text: "working".into(),
                })
            };
            let done = || EventBody::RunCompleted(RunCompleted { summary: None });
            match behavior {
                Behavior::Complete => {
                    sink.emit(delta()).expect("delta");
                    sink.emit(done()).expect("completed");
                }
                Behavior::WaitForRelease(gate) => {
                    gate.notified().await;
                    sink.emit(done()).expect("completed");
                }
                Behavior::ObeyCancel => {
                    sink.emit(delta()).expect("delta");
                    cancel.cancelled().await;
                }
                Behavior::IgnoreCancel => loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                },
                Behavior::ReturnEarly => {
                    sink.emit(delta()).expect("delta");
                }
                Behavior::TryIllegal => {
                    if sink.emit(EventBody::RunResumed(RunResumed {})).is_err() {
                        script.illegal_refused.fetch_add(1, Ordering::SeqCst);
                    }
                    sink.emit(done()).expect("completed");
                }
            }
        })
    }
}

fn request(run: &str) -> RunRequest {
    RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("agent"),
        session_id: SessionId::from(format!("session-{run}").as_str()),
        prompt: "a task".into(),
    }
}

struct Harness {
    sup: Supervisor,
    log: Arc<MemLog>,
    script: Arc<Script>,
}

fn harness(max_concurrent: usize, behaviors: &[(&str, Behavior)]) -> Harness {
    let script = Arc::new(Script::default());
    {
        let mut b = script.behaviors.lock().expect("behaviors");
        for (run, behavior) in behaviors {
            b.insert((*run).to_owned(), behavior.clone());
        }
    }
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig {
            max_concurrent,
            force_drop_after: Duration::from_secs(2),
        },
        Arc::new(ScriptDriver(script.clone())),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    Harness { sup, log, script }
}

/// Recall-all, failing the test (instead of hanging it) if it doesn't return within 10 s of
/// virtual time.
async fn recall_all_bounded(sup: &Supervisor) -> corral_core::supervisor::RecallReport {
    tokio::time::timeout(Duration::from_secs(10), sup.recall_all())
        .await
        .expect("Recall-all must return; a driver that ignores cancel has to be dropped")
}

/// Lets spawned tasks run until nothing is left to do right now.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

// ---- TP-SUP-01 ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_sup_01_the_concurrency_limit_queues_extra_runs_fifo() {
    let gates: Vec<(String, Arc<Notify>)> = (1..=5)
        .map(|i| (format!("r{i}"), Arc::new(Notify::new())))
        .collect();
    let behaviors: Vec<(&str, Behavior)> = gates
        .iter()
        .map(|(r, g)| (r.as_str(), Behavior::WaitForRelease(g.clone())))
        .collect();
    let h = harness(2, &behaviors);

    for (run, _) in &gates {
        h.sup.deploy(request(run)).expect("deploy");
    }
    settle().await;
    assert_eq!(h.sup.running(), 2);
    assert_eq!(
        h.sup.queued(),
        vec![RunId::from("r3"), RunId::from("r4"), RunId::from("r5")]
    );
    assert_eq!(
        h.log.kinds("r3"),
        ["run.queued"],
        "a queued run hasn't started"
    );

    // Finish r2 first: the oldest queued run (r3) takes its slot, not r5.
    gates[1].1.notify_one();
    settle().await;
    assert_eq!(
        *h.script.started.lock().expect("started"),
        ["r1", "r2", "r3"]
    );

    for (_, gate) in &gates {
        gate.notify_one();
        settle().await;
    }
    for (_, gate) in &gates {
        gate.notify_one();
    }
    h.sup.wait_idle().await;
    assert_eq!(
        *h.script.started.lock().expect("started"),
        ["r1", "r2", "r3", "r4", "r5"]
    );
    for (run, _) in &gates {
        assert_eq!(
            h.log.kinds(run),
            ["run.queued", "run.started", "run.completed"]
        );
    }
    h.log.assert_every_run_is_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_sup_01_runs_are_logged_legally_even_when_drivers_misbehave() {
    let h = harness(
        8,
        &[
            ("ok", Behavior::Complete),
            ("early", Behavior::ReturnEarly),
            ("illegal", Behavior::TryIllegal),
        ],
    );
    for run in ["ok", "early", "illegal"] {
        h.sup.deploy(request(run)).expect("deploy");
    }
    h.sup.wait_idle().await;

    assert_eq!(
        h.log.kinds("ok"),
        ["run.queued", "run.started", "model.delta", "run.completed"]
    );
    // A driver that stops without finishing fails the run instead of leaving it hanging.
    assert_eq!(
        h.log.kinds("early"),
        ["run.queued", "run.started", "model.delta", "run.failed"]
    );
    let early = h.log.run("early");
    let EventBody::RunFailed(f) = &early.last().expect("event").body else {
        panic!("expected run.failed");
    };
    assert_eq!(f.reason, FailureReason::Adapter);
    // An illegal event is refused and never reaches the log.
    assert_eq!(h.script.illegal_refused.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.log.kinds("illegal"),
        ["run.queued", "run.started", "run.completed"]
    );
    h.log.assert_every_run_is_legal();

    // Deploying a run ID twice is refused.
    assert!(h.sup.deploy(request("ok")).is_err());
}

// ---- TP-SUP-02 ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tp_sup_02_recall_all_cancels_running_and_queued_runs() {
    let runs: Vec<String> = (1..=10).map(|i| format!("r{i}")).collect();
    let behaviors: Vec<(&str, Behavior)> = runs
        .iter()
        .map(|r| (r.as_str(), Behavior::ObeyCancel))
        .collect();
    let h = harness(8, &behaviors);
    for run in &runs {
        h.sup.deploy(request(run)).expect("deploy");
    }
    settle().await;
    assert_eq!(h.sup.running(), 8);
    assert_eq!(h.sup.queued().len(), 2);

    let start = Instant::now();
    let report = recall_all_bounded(&h.sup).await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "well-behaved runs stop at once"
    );
    assert_eq!(report.stopped, 10);
    assert_eq!(report.forced, 0);
    assert_eq!(h.sup.running(), 0);
    assert!(h.sup.queued().is_empty());

    for run in &runs {
        let events = h.log.run(run);
        let EventBody::RunCancelled(c) = &events.last().expect("event").body else {
            panic!("{run}: expected run.cancelled");
        };
        assert_eq!(c.by, CancelledBy::RecallAll);
    }
    // The two queued runs never started.
    assert_eq!(h.log.kinds("r9"), ["run.queued", "run.cancelled"]);
    assert_eq!(h.script.started.lock().expect("started").len(), 8);
    h.log.assert_every_run_is_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_sup_02_a_driver_that_ignores_cancel_is_force_dropped_at_two_seconds() {
    let h = harness(
        8,
        &[
            ("stubborn-1", Behavior::IgnoreCancel),
            ("stubborn-2", Behavior::IgnoreCancel),
            ("polite", Behavior::ObeyCancel),
        ],
    );
    for run in ["stubborn-1", "stubborn-2", "polite"] {
        h.sup.deploy(request(run)).expect("deploy");
    }
    settle().await;

    let start = Instant::now();
    let report = recall_all_bounded(&h.sup).await;
    let took = start.elapsed();
    assert_eq!(
        took,
        Duration::from_secs(2),
        "force-drop happens at exactly 2 s"
    );
    assert_eq!(report.stopped, 3);
    assert_eq!(report.forced, 2);
    assert_eq!(
        h.script.dropped.load(Ordering::SeqCst),
        3,
        "every driver future was dropped"
    );

    for run in ["stubborn-1", "stubborn-2", "polite"] {
        assert_eq!(
            h.log.kinds(run).last(),
            Some(&"run.cancelled"),
            "{run} ends cancelled"
        );
    }
    h.log.assert_every_run_is_legal();
    assert_eq!(
        h.sup.phase(&RunId::from("stubborn-1")),
        Some(Phase::Cancelled)
    );
}

#[tokio::test(start_paused = true)]
async fn tp_sup_02_stop_cancels_one_run_and_frees_its_slot() {
    let h = harness(
        1,
        &[
            ("first", Behavior::ObeyCancel),
            ("second", Behavior::Complete),
        ],
    );
    h.sup.deploy(request("first")).expect("deploy");
    h.sup.deploy(request("second")).expect("deploy");
    settle().await;
    assert_eq!(h.sup.queued(), vec![RunId::from("second")]);

    h.sup.cancel(&RunId::from("first")).await.expect("cancel");
    h.sup.wait_idle().await;

    let first = h.log.run("first");
    let EventBody::RunCancelled(c) = &first.last().expect("event").body else {
        panic!("expected run.cancelled");
    };
    assert_eq!(c.by, CancelledBy::User);
    assert_eq!(h.log.kinds("second").last(), Some(&"run.completed"));
    h.log.assert_every_run_is_legal();
}
