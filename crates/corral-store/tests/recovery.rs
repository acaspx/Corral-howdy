//! Crash recovery (TP-SUP-03) and the store as the supervisor's event log.

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::lifecycle::{Lifecycle, next};
use corral_core::supervisor::{
    CancelToken, EventSink, RunDriver, RunFuture, RunRequest, Supervisor, SupervisorConfig,
};
use corral_store::{Integrity, Store, StoreError};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("corral-recovery-{}-{name}-{n}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear old test folder");
        }
        std::fs::create_dir_all(&dir).expect("create test folder");
        Self(dir)
    }

    fn path(&self) -> PathBuf {
        self.0.join("corral.db")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder doesn't affect the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Appends `bodies` to `run` as seq 1, 2, 3…
fn write(store: &Store, run: &str, bodies: Vec<EventBody>) {
    for (i, body) in bodies.into_iter().enumerate() {
        let seq = i as u64 + 1;
        store
            .append(&Event {
                run_id: RunId::from(run),
                seq,
                ts: 100 * seq,
                span_id: SpanId::from(format!("{run}:invoke_agent").as_str()),
                parent_span_id: None,
                body,
            })
            .expect("append");
    }
}

fn queued(run: &str) -> EventBody {
    EventBody::RunQueued(RunQueued {
        agent_id: AgentId::from("agent"),
        session_id: SessionId::from(format!("s-{run}").as_str()),
        prompt: "a task".into(),
    })
}
fn started() -> EventBody {
    EventBody::RunStarted(RunStarted {
        adapter: "fake".into(),
    })
}
fn delta() -> EventBody {
    EventBody::ModelDelta(ModelDelta { text: "hi".into() })
}
fn completed() -> EventBody {
    EventBody::RunCompleted(RunCompleted { summary: None })
}

fn assert_legal(store: &Store, run: &str) {
    let mut state = Lifecycle::idle();
    for e in store.read_run(&RunId::from(run)).expect("read") {
        state = next(&state, &e).unwrap_or_else(|err| panic!("{run}: {err}"));
    }
}

#[test]
fn tp_sup_03_runs_left_unfinished_are_failed_with_app_restarted() {
    let db = TempDb::new("orphans");
    let store = Store::open(&db.path()).expect("open");

    // Left mid-flight by a crash or force-quit:
    write(&store, "only-queued", vec![queued("only-queued")]);
    write(
        &store,
        "running",
        vec![queued("running"), started(), delta()],
    );
    write(
        &store,
        "waiting",
        vec![
            queued("waiting"),
            started(),
            EventBody::CheckpointOpened(CheckpointOpened {
                checkpoint_id: CheckpointId::from("cp-1"),
                tool_call_id: ToolCallId::from("call-1"),
                action_class: ActionClass::CommsSend,
                risk: Risk::High,
                title: "Send an email".into(),
                detail: "To: someone@example.test".into(),
            }),
        ],
    );
    write(
        &store,
        "paused",
        vec![
            queued("paused"),
            started(),
            EventBody::RunPaused(RunPaused {
                reason: PauseReason::User,
            }),
        ],
    );
    // Finished before the crash: left alone.
    write(
        &store,
        "completed",
        vec![queued("completed"), started(), completed()],
    );
    write(
        &store,
        "failed",
        vec![
            queued("failed"),
            started(),
            EventBody::RunFailed(RunFailed {
                reason: FailureReason::Provider,
                message: "provider error".into(),
            }),
        ],
    );
    write(
        &store,
        "cancelled",
        vec![
            queued("cancelled"),
            EventBody::RunCancelled(RunCancelled {
                by: CancelledBy::RecallAll,
            }),
        ],
    );
    write(
        &store,
        "sent-back",
        vec![
            queued("sent-back"),
            started(),
            completed(),
            EventBody::SessionClosed(SessionClosed {}),
        ],
    );
    drop(store);

    // Relaunch.
    let store = Store::open(&db.path()).expect("reopen");
    let failed = store.fail_orphaned_runs(9_000).expect("recover");
    assert_eq!(
        failed,
        ["only-queued", "paused", "running", "waiting"].map(RunId::from)
    );

    for run in ["only-queued", "running", "waiting", "paused"] {
        let events = store.read_run(&RunId::from(run)).expect("read");
        let last = events.last().expect("event");
        let EventBody::RunFailed(f) = &last.body else {
            panic!("{run}: expected run.failed");
        };
        assert_eq!(f.reason, FailureReason::AppRestarted, "{run}");
        assert_eq!(last.ts, 9_000);
        assert_eq!(last.seq, events.len() as u64, "{run}: next seq in order");
        assert_legal(&store, run);
    }
    for (run, len) in [
        ("completed", 3),
        ("failed", 3),
        ("cancelled", 2),
        ("sent-back", 4),
    ] {
        assert_eq!(
            store.read_run(&RunId::from(run)).expect("read").len(),
            len,
            "{run} is untouched"
        );
    }

    // Idempotent, and the chains still verify after the extra events.
    assert!(store.fail_orphaned_runs(9_500).expect("again").is_empty());
    drop(store);
    assert_eq!(
        Store::open(&db.path()).expect("reopen").integrity(),
        &Integrity::Verified
    );
}

#[test]
fn tp_sup_03_a_read_only_store_refuses_recovery() {
    let db = TempDb::new("read-only");
    let store = Store::open(&db.path()).expect("open");
    write(&store, "r", vec![queued("r"), started()]);
    drop(store);

    let raw = rusqlite::Connection::open(db.path()).expect("raw open");
    raw.execute_batch(
        "DROP TRIGGER events_no_update; UPDATE events SET ts = 1 WHERE run_id = 'r' AND seq = 2;",
    )
    .expect("tamper");
    drop(raw);

    let store = Store::open(&db.path()).expect("open");
    assert!(store.is_read_only());
    assert!(matches!(
        store.fail_orphaned_runs(9_000),
        Err(StoreError::ReadOnly)
    ));
}

// ---- The store as the supervisor's event log --------------------------------------------------

struct Answer;

impl RunDriver for Answer {
    fn run(&self, _req: RunRequest, sink: EventSink, _cancel: CancelToken) -> RunFuture {
        Box::pin(async move {
            sink.emit(EventBody::ModelDelta(ModelDelta {
                text: "Here's the answer.".into(),
            }))
            .expect("delta");
            sink.emit(EventBody::RunCompleted(RunCompleted {
                summary: Some("Answered.".into()),
            }))
            .expect("completed");
        })
    }
}

#[tokio::test]
async fn the_supervisor_writes_runs_to_the_real_store() {
    let db = TempDb::new("supervisor");
    let store = Arc::new(Store::open(&db.path()).expect("open"));
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(Answer),
        store.clone(),
        Arc::new(ManualClock::at(5_000)),
    );
    for run in ["a", "b"] {
        sup.deploy(RunRequest {
            run_id: RunId::from(run),
            agent_id: AgentId::from("grok"),
            session_id: SessionId::from(format!("s-{run}").as_str()),
            prompt: "a question".into(),
        })
        .expect("deploy");
    }
    sup.wait_idle().await;

    for run in ["a", "b"] {
        let kinds: Vec<&str> = store
            .read_run(&RunId::from(run))
            .expect("read")
            .iter()
            .map(|e| e.kind().as_str())
            .collect();
        assert_eq!(
            kinds,
            ["run.queued", "run.started", "model.delta", "run.completed"]
        );
        assert_legal(&store, run);
    }
    drop(sup);
    drop(store);
    assert_eq!(
        Store::open(&db.path()).expect("reopen").integrity(),
        &Integrity::Verified
    );
}
