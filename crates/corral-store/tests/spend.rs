//! This month's spend comes back from the log at launch (TP-BUD-07): `Store::spend_since` reads
//! every `usage.reported` since a moment, with whose run it was, in log order, in one query. The
//! budget's `SpendLedger` is seeded from it.

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_store::{Spend, Store};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("corral-spend-{}-{name}-{n}", std::process::id()));
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

/// Appends `bodies` to `run` as seq 1, 2, 3…, at the given times.
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

fn queued(agent: &str) -> EventBody {
    EventBody::RunQueued(RunQueued {
        agent_id: AgentId::from(agent),
        session_id: SessionId::from("s1"),
        prompt: "a task".into(),
    })
}

fn started() -> EventBody {
    EventBody::RunStarted(RunStarted {
        adapter: "fake".into(),
    })
}

fn request() -> EventBody {
    EventBody::ModelRequest(ModelRequest {
        provider: "test".into(),
        model: "test-model".into(),
    })
}

fn usage(cost: u64) -> EventBody {
    EventBody::UsageReported(UsageReported {
        input_tokens: 10,
        output_tokens: 10,
        cost_micro_usd: cost,
    })
}

fn spend(agent: &str, ts_ms: u64, cost_micro_usd: u64) -> Spend {
    Spend {
        agent: AgentId::from(agent),
        ts_ms,
        cost_micro_usd,
    }
}

#[test]
fn tp_bud_07_spend_since_a_moment_comes_back_with_whose_run_it_was_in_log_order() {
    let db = TempDb::new("order");
    let store = Store::open(&db.path()).expect("open");
    write(
        &store,
        "r1",
        vec![
            (100, queued("scout")),
            (110, started()),
            (120, request()),
            (130, usage(300)),
            (140, request()),
            (150, usage(50)),
        ],
    );
    write(
        &store,
        "r2",
        vec![
            (200, queued("grok")),
            (210, started()),
            (220, request()),
            (230, usage(700)),
        ],
    );
    assert_eq!(
        store.spend_since(0).expect("read"),
        [
            spend("scout", 130, 300),
            spend("scout", 150, 50),
            spend("grok", 230, 700),
        ]
    );
    // From a moment on: earlier usage is left out, even in a run that started before it.
    assert_eq!(
        store.spend_since(150).expect("read"),
        [spend("scout", 150, 50), spend("grok", 230, 700)]
    );
    assert_eq!(store.spend_since(231).expect("read"), []);
}

#[test]
fn tp_bud_07_an_empty_log_has_spent_nothing() {
    let db = TempDb::new("empty");
    let store = Store::open(&db.path()).expect("open");
    assert_eq!(store.spend_since(0).expect("read"), []);
}
