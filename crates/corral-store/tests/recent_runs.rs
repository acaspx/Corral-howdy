//! The trace viewer's list of runs (M4 item 5, TP-TV-10): `Store::recent_runs` reads the newest
//! runs, newest first, with who ran them, the prompt, when they were queued and ended, how they
//! ended and what they cost, in one query.

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_store::{MAX_RECENT_RUNS, RunOutcome, Store};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("corral-recent-{}-{name}-{n}", std::process::id()));
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

fn write(store: &Store, run: &str, bodies: Vec<(u64, EventBody)>) {
    for (i, (ts, body)) in bodies.into_iter().enumerate() {
        store
            .append(&Event {
                run_id: RunId::from(run),
                seq: i as u64 + 1,
                ts,
                span_id: SpanId::from(format!("{run}:invoke_agent").as_str()),
                parent_span_id: None,
                body,
            })
            .expect("append");
    }
}

fn queued(agent: &str, prompt: &str) -> EventBody {
    EventBody::RunQueued(RunQueued {
        agent_id: AgentId::from(agent),
        session_id: SessionId::from("s1"),
        prompt: prompt.into(),
    })
}

fn usage(cost: u64) -> EventBody {
    EventBody::UsageReported(UsageReported {
        input_tokens: 10,
        output_tokens: 2,
        cost_micro_usd: cost,
    })
}

#[test]
fn tp_tv_10_recent_runs_come_newest_first_with_how_they_ended() {
    let db = TempDb::new("list");
    let store = Store::open(&db.path()).expect("open");
    write(
        &store,
        "old",
        vec![
            (1_000, queued("grok", "first")),
            (1_100, usage(300)),
            (1_200, usage(200)),
            (
                1_300,
                EventBody::RunCompleted(RunCompleted { summary: None }),
            ),
        ],
    );
    write(
        &store,
        "failed",
        vec![
            (2_000, queued("claude", "second")),
            (
                2_500,
                EventBody::RunFailed(RunFailed {
                    reason: FailureReason::Provider,
                    message: "overloaded".into(),
                }),
            ),
        ],
    );
    write(
        &store,
        "stopped",
        vec![
            (3_000, queued("claude", "third")),
            (
                3_100,
                EventBody::RunCancelled(RunCancelled {
                    by: CancelledBy::User,
                }),
            ),
        ],
    );
    write(
        &store,
        "going",
        vec![
            (4_000, queued("grok", "fourth")),
            (
                4_001,
                EventBody::RunStarted(RunStarted {
                    adapter: "xai".into(),
                }),
            ),
        ],
    );

    let runs = store.recent_runs(10).expect("list");
    let ids: Vec<&str> = runs.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(ids, ["going", "stopped", "failed", "old"]);

    let old = &runs[3];
    assert_eq!(old.agent_id, AgentId::from("grok"));
    assert_eq!(old.prompt, "first");
    assert_eq!((old.queued_ms, old.ended_ms), (1_000, Some(1_300)));
    assert_eq!(old.outcome, RunOutcome::Completed);
    assert_eq!(old.cost_micro_usd, 500);

    assert_eq!(runs[2].outcome, RunOutcome::Failed);
    assert_eq!(runs[2].ended_ms, Some(2_500));
    assert_eq!(runs[1].outcome, RunOutcome::Cancelled);
    assert_eq!(
        (runs[0].outcome, runs[0].ended_ms),
        (RunOutcome::Running, None)
    );
    assert_eq!(runs[0].cost_micro_usd, 0);
}

#[test]
fn tp_tv_10_the_list_is_bounded() {
    let db = TempDb::new("bounded");
    let store = Store::open(&db.path()).expect("open");
    for n in 0..5_u64 {
        write(
            &store,
            &format!("r{n}"),
            vec![(1_000 + n, queued("grok", "task"))],
        );
    }
    let two = store.recent_runs(2).expect("list");
    let ids: Vec<&str> = two.iter().map(|r| r.run_id.as_str()).collect();
    assert_eq!(ids, ["r4", "r3"]);
    assert_eq!(store.recent_runs(0).expect("list").len(), 0);
    assert_eq!(MAX_RECENT_RUNS, 200);
    assert_eq!(
        store.recent_runs(usize::MAX).expect("list").len(),
        5,
        "asking for more than the most is the most"
    );
}

#[test]
fn tp_tv_10_an_empty_log_lists_nothing() {
    let db = TempDb::new("empty");
    let store = Store::open(&db.path()).expect("open");
    assert!(store.recent_runs(10).expect("list").is_empty());
}
