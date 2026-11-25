//! Pause and resume (docs/01 §4 lifecycle, EV-STOP-03; TP-SUP-04..05). Pausing asks the driver
//! to stop at its next safe point, between steps and never mid-request: there it logs
//! `run.paused` and waits, logging nothing else, until Resume (`run.resumed`, and it carries on)
//! or Stop. Only a run that's running can pause; Resume before the safe point means it never
//! pauses.
//!
//! Time is tokio's paused clock.

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::lifecycle::{Lifecycle, Phase, next};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig, SupervisorError,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{Instant, sleep};

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn run(&self, run: &str) -> Vec<Event> {
        let id = RunId::from(run);
        let all = self.0.lock().expect("log");
        all.iter().filter(|e| e.run_id == id).cloned().collect()
    }

    fn kinds(&self, run: &str) -> Vec<&'static str> {
        self.run(run).iter().map(|e| e.kind().as_str()).collect()
    }

    fn assert_legal(&self, run: &str) {
        let mut state = Lifecycle::idle();
        for e in self.run(run) {
            state = next(&state, &e).unwrap_or_else(|err| panic!("{run}: illegal event: {err}"));
        }
    }
}

/// Three steps of one second each, with a safe point before each, then done.
struct Steps;

impl RunDriver for Steps {
    fn run(&self, _req: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        Box::pin(async move {
            for step in 1..=3 {
                if !sink.pause_point(&cancel).await {
                    return;
                }
                sink.emit(EventBody::ModelDelta(ModelDelta {
                    text: format!("step {step}"),
                }))
                .expect("delta");
                tokio::select! {
                    () = sleep(Duration::from_secs(1)) => {}
                    () = cancel.cancelled() => return,
                }
            }
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("completed");
        })
    }
}

fn supervisor(max_concurrent: usize) -> (Supervisor, Arc<MemLog>) {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig {
            max_concurrent,
            ..SupervisorConfig::default()
        },
        Arc::new(Steps),
        log.clone(),
        Arc::new(corral_core::clock::ManualClock::at(1_000)),
    );
    (sup, log)
}

fn deploy(sup: &Supervisor, run: &str) {
    sup.deploy(RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from("scout"),
        session_id: SessionId::from(format!("s-{run}").as_str()),
        prompt: "Tidy my notes.".into(),
    })
    .expect("deploy");
}

fn r(run: &str) -> RunId {
    RunId::from(run)
}

async fn idle(sup: &Supervisor) {
    tokio::time::timeout(Duration::from_secs(3600), sup.wait_idle())
        .await
        .expect("the runs end");
}

const DONE: [&str; 8] = [
    "run.queued",
    "run.started",
    "model.delta",
    "run.paused",
    "run.resumed",
    "model.delta",
    "model.delta",
    "run.completed",
];

// ---- TP-SUP-04: pause at the next safe point; resume carries on ---------------------------

#[tokio::test(start_paused = true)]
async fn tp_sup_04_pause_takes_hold_at_the_next_safe_point_and_resume_carries_on() {
    let (sup, log) = supervisor(8);
    deploy(&sup, "r1");
    sleep(Duration::from_millis(500)).await;
    sup.pause(&r("r1")).expect("pause");
    assert_eq!(
        log.kinds("r1"),
        ["run.queued", "run.started", "model.delta"],
        "the step under way isn't cut short"
    );
    sleep(Duration::from_secs(600)).await;
    assert_eq!(
        log.kinds("r1"),
        ["run.queued", "run.started", "model.delta", "run.paused"],
        "paused at the safe point, and nothing more while paused"
    );
    assert_eq!(sup.phase(&r("r1")), Some(Phase::Paused));
    assert!(matches!(
        log.run("r1")[3].body,
        EventBody::RunPaused(RunPaused {
            reason: PauseReason::User
        })
    ));
    sup.resume(&r("r1")).expect("resume");
    idle(&sup).await;
    assert_eq!(log.kinds("r1"), DONE);
    log.assert_legal("r1");
}

#[tokio::test(start_paused = true)]
async fn tp_sup_04_resume_before_the_safe_point_means_it_never_pauses() {
    let (sup, log) = supervisor(8);
    deploy(&sup, "r1");
    sleep(Duration::from_millis(500)).await;
    sup.pause(&r("r1")).expect("pause");
    sleep(Duration::from_millis(100)).await;
    sup.resume(&r("r1")).expect("resume");
    idle(&sup).await;
    assert!(
        !log.kinds("r1").contains(&"run.paused"),
        "{:?}",
        log.kinds("r1")
    );
    assert_eq!(log.kinds("r1").last(), Some(&"run.completed"));
}

#[tokio::test(start_paused = true)]
async fn tp_sup_04_pausing_twice_pauses_once_and_resuming_a_running_run_does_nothing() {
    let (sup, log) = supervisor(8);
    deploy(&sup, "r1");
    sleep(Duration::from_millis(100)).await;
    sup.resume(&r("r1"))
        .expect("a running run can be resumed: nothing happens");
    sup.pause(&r("r1")).expect("pause");
    sup.pause(&r("r1")).expect("again");
    sleep(Duration::from_secs(5)).await;
    sup.pause(&r("r1")).expect("while paused");
    sup.resume(&r("r1")).expect("resume");
    idle(&sup).await;
    assert_eq!(log.kinds("r1"), DONE);
}

#[tokio::test(start_paused = true)]
async fn tp_sup_04_only_a_running_run_can_pause() {
    let (sup, _log) = supervisor(1);
    deploy(&sup, "r1");
    deploy(&sup, "r2");
    assert!(
        matches!(
            sup.pause(&r("r2")),
            Err(SupervisorError::NotRunning(id)) if id == r("r2")
        ),
        "a queued run"
    );
    assert!(matches!(
        sup.resume(&r("r2")),
        Err(SupervisorError::NotRunning(_))
    ));
    assert!(matches!(
        sup.pause(&r("nobody")),
        Err(SupervisorError::UnknownRun(_))
    ));
    idle(&sup).await;
    assert!(
        matches!(sup.pause(&r("r1")), Err(SupervisorError::NotRunning(_))),
        "an ended run"
    );
}

// ---- TP-SUP-05: Stop while paused ---------------------------------------------------------

async fn paused(sup: &Supervisor) {
    deploy(sup, "r1");
    sleep(Duration::from_millis(500)).await;
    sup.pause(&r("r1")).expect("pause");
    sleep(Duration::from_secs(5)).await;
    assert_eq!(sup.phase(&r("r1")), Some(Phase::Paused));
}

#[tokio::test(start_paused = true)]
async fn tp_sup_05_stop_while_paused_ends_the_run_at_once() {
    let (sup, log) = supervisor(8);
    paused(&sup).await;
    let pressed = Instant::now();
    sup.cancel(&r("r1")).await.expect("stop");
    assert_eq!(pressed.elapsed(), Duration::ZERO, "no force-drop wait");
    assert_eq!(
        log.kinds("r1"),
        [
            "run.queued",
            "run.started",
            "model.delta",
            "run.paused",
            "run.cancelled"
        ]
    );
    log.assert_legal("r1");
}

#[tokio::test(start_paused = true)]
async fn tp_sup_05_stop_all_while_paused_drops_nothing() {
    let (sup, log) = supervisor(8);
    paused(&sup).await;
    let report = sup.recall_all().await;
    assert_eq!((report.stopped, report.forced), (1, 0));
    assert!(matches!(
        log.run("r1").last().map(|e| &e.body),
        Some(EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::RecallAll
        }))
    ));
}

/// Opens a checkpoint, tries a safe point while it's open, resolves it, and tries again.
struct AtCheckpoint(Arc<Mutex<Vec<bool>>>);

impl RunDriver for AtCheckpoint {
    fn run(&self, _req: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let answers = Arc::clone(&self.0);
        Box::pin(async move {
            use corral_core::domain::{ActionClass, Risk};
            use corral_core::ids::{CheckpointId, ToolCallId};
            sink.emit(EventBody::CheckpointOpened(CheckpointOpened {
                checkpoint_id: CheckpointId::from("cp-1"),
                tool_call_id: ToolCallId::from("call-1"),
                action_class: ActionClass::WriteLocal,
                risk: Risk::Normal,
                title: "Saving a file".into(),
                detail: "notes.md".into(),
            }))
            .expect("opened");
            sleep(Duration::from_secs(1)).await;
            let during = sink.pause_point(&cancel).await;
            answers.lock().expect("answers").push(during);
            sink.emit(EventBody::CheckpointResolved(CheckpointResolved {
                checkpoint_id: CheckpointId::from("cp-1"),
                resolution: Resolution::Approved,
            }))
            .expect("resolved");
            let after = sink.pause_point(&cancel).await;
            answers.lock().expect("answers").push(after);
            sink.emit(EventBody::RunCompleted(RunCompleted { summary: None }))
                .expect("completed");
        })
    }
}

#[tokio::test(start_paused = true)]
async fn tp_sup_04_a_pause_asked_for_at_a_checkpoint_takes_hold_after_it() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(AtCheckpoint(Arc::clone(&answers))),
        log.clone(),
        Arc::new(corral_core::clock::ManualClock::at(1_000)),
    );
    deploy(&sup, "r1");
    sleep(Duration::from_millis(500)).await;
    sup.pause(&r("r1")).expect("pause while asking");
    sleep(Duration::from_secs(5)).await;
    assert_eq!(
        *answers.lock().expect("answers"),
        [true],
        "a safe point while the checkpoint is open carries on and logs nothing"
    );
    assert_eq!(
        log.kinds("r1").last(),
        Some(&"run.paused"),
        "then pauses after it"
    );
    sup.resume(&r("r1")).expect("resume");
    idle(&sup).await;
    assert_eq!(*answers.lock().expect("answers"), [true, true]);
    assert_eq!(
        log.kinds("r1"),
        [
            "run.queued",
            "run.started",
            "checkpoint.opened",
            "checkpoint.resolved",
            "run.paused",
            "run.resumed",
            "run.completed"
        ]
    );
    log.assert_legal("r1");
}
