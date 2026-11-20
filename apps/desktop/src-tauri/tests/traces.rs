//! The trace viewer's commands (M4 item 5, part 2, TP-TV-11, 12). `Traces` holds the event store
//! once the corral opens it; the window asks it for the newest runs and for one run's span tree,
//! with the run's events as JSON to copy. Before the store is open, both say so.

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_desktop_lib::traces::{MAX_PROMPT_CHARS, RECENT_RUNS, Traces};
use corral_store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("corral-traces-{}-{name}-{n}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear old test folder");
        }
        std::fs::create_dir_all(&dir).expect("create test folder");
        Self(dir)
    }

    fn store(&self) -> Arc<Store> {
        Arc::new(Store::open(&self.0.join("corral.db")).expect("open"))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder doesn't affect the result.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(store: &Store, run: &str, agent: &str, prompt: &str, ended: bool) {
    let mut bodies = vec![
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from(agent),
            session_id: SessionId::from("s1"),
            prompt: prompt.into(),
        }),
        EventBody::RunStarted(RunStarted {
            adapter: "xai.responses".into(),
        }),
        EventBody::ModelRequest(ModelRequest {
            provider: "xai".into(),
            model: "grok-4.7".into(),
        }),
    ];
    if ended {
        bodies.push(EventBody::ModelResponse(ModelResponse {
            text: "Done.".into(),
            finish_reason: Some("stop".into()),
        }));
        bodies.push(EventBody::UsageReported(UsageReported {
            input_tokens: 10,
            output_tokens: 2,
            cost_micro_usd: 1_500,
        }));
        bodies.push(EventBody::RunCompleted(RunCompleted { summary: None }));
    }
    for (i, body) in bodies.into_iter().enumerate() {
        store
            .append(&Event {
                run_id: RunId::from(run),
                seq: i as u64 + 1,
                ts: 1_000 + i as u64,
                span_id: SpanId::from(format!("{run}:invoke_agent").as_str()),
                parent_span_id: None,
                body,
            })
            .expect("append");
    }
}

#[test]
fn tp_tv_11_before_the_store_is_open_the_viewer_says_so() {
    let traces = Traces::default();
    let err = traces.recent_runs().expect_err("not open");
    assert!(err.contains("isn't open yet"), "{err}");
    assert!(
        traces
            .run_trace("r1")
            .expect_err("not open")
            .contains("isn't open yet")
    );
}

#[test]
fn tp_tv_11_the_list_names_each_agent_and_cleans_the_prompt() {
    let db = TempDb::new("list");
    let store = db.store();
    write(&store, "r1", "grok", "Tidy my notes", true);
    let long = format!("Plan\u{202e} my week {}", "x".repeat(MAX_PROMPT_CHARS));
    write(&store, "r2", "claude", &long, false);
    write(&store, "r3", "someone-else", "hi", false);
    let traces = Traces::default();
    traces.attach(store);

    let runs = traces.recent_runs().expect("list");
    let json = serde_json::to_value(&runs).expect("json");
    assert_eq!(json.as_array().map(Vec::len), Some(3));
    let grok = &json[2];
    assert_eq!(grok["runId"], "r1");
    assert_eq!(grok["agentName"], "Grok");
    assert_eq!(grok["prompt"], "Tidy my notes");
    assert_eq!(grok["outcome"], "completed");
    assert_eq!(grok["costMicroUsd"], 1_500);
    assert_eq!(grok["queuedMs"], 1_000);
    assert_eq!(grok["endedMs"], 1_005);
    let claude = &json[1];
    assert_eq!(claude["agentName"], "Claude");
    assert_eq!(claude["outcome"], "running");
    assert_eq!(claude["endedMs"], serde_json::Value::Null);
    let prompt = claude["prompt"].as_str().expect("prompt");
    assert!(!prompt.contains('\u{202e}'), "cleaned: {prompt:?}");
    assert!(prompt.chars().count() <= MAX_PROMPT_CHARS, "capped");
    assert_eq!(
        json[0]["agentName"], "someone-else",
        "an agent Corral doesn't know keeps its ID"
    );
    assert_eq!(RECENT_RUNS, 100);
}

#[test]
fn tp_tv_12_a_runs_trace_comes_with_its_events_as_json() {
    let db = TempDb::new("trace");
    let store = db.store();
    write(&store, "r1", "grok", "Tidy my notes", true);
    let traces = Traces::default();
    traces.attach(Arc::clone(&store));

    let trace = traces.run_trace("r1").expect("trace");
    let ids: Vec<&str> = trace.view.spans.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["run", "model-1"]);
    let events: serde_json::Value = serde_json::from_str(&trace.json).expect("valid JSON");
    let events = events.as_array().expect("an array of events");
    assert_eq!(events.len(), 6);
    assert_eq!(events[0]["kind"], "run.queued");
    assert_eq!(events[5]["kind"], "run.completed");
    assert!(trace.json.contains("\n  "), "indented, for reading");
    let shape = serde_json::to_value(&trace).expect("json");
    assert_eq!(shape["view"]["runId"], "r1");
    assert!(shape["json"].is_string());
}

#[test]
fn tp_tv_12_a_run_that_isnt_in_the_log_is_an_error() {
    let db = TempDb::new("missing");
    let traces = Traces::default();
    traces.attach(db.store());
    let err = traces.run_trace("nope").expect_err("missing");
    assert!(err.contains("No run"), "{err}");
}
