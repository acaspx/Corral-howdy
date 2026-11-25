//! The trace viewer's story fixtures, from real event streams (M4 item 7; TP-SB-01). Each run is
//! played by the FakeAdapter through a real supervisor, the tool gate and a checkpoint desk into
//! a real event store, and the viewer's own reads (`Traces::recent_runs`, `Traces::run_trace`)
//! turn the log into what the window shows. The result is committed as
//! `apps/desktop/src/fixtures/traces.json`, so Storybook shows what the core produces, and this
//! test fails when the core changes and the fixtures don't: regenerate them with
//! `CORRAL_WRITE_FIXTURES=1 cargo test -p corral-desktop --test story_fixtures`.

use corral_adapters::fake::{FakeDriver, FakeScript, ToolSetup};
use corral_core::budget::BudgetConfig;
use corral_core::clock::Clock;
use corral_core::domain::ActionClass;
use corral_core::event::FailureReason;
use corral_core::ids::{AgentId, RunId, SessionId};
use corral_core::supervisor::{RunRequest, Supervisor, SupervisorConfig};
use corral_desktop_lib::traces::Traces;
use corral_policy::{Approval, Policy, Profile, Scope};
use corral_store::Store;
use corral_tools::{Answer, BoxFuture, CheckpointDesk, Tool, ToolError, ToolGateway};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 2026-11-24 09:00 Pacific, in ms.
const START_MS: u64 = 1_795_971_600_000;

/// Unix time that moves with tokio's paused clock, so the waterfall has real widths and the
/// fixtures are the same on every run.
struct PausedClock(tokio::time::Instant);

impl Clock for PausedClock {
    fn now_ms(&self) -> u64 {
        let elapsed = u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX);
        START_MS.saturating_add(elapsed)
    }
}

/// A tool that answers at once.
struct Plain {
    name: &'static str,
    class: ActionClass,
    answer: &'static str,
}

impl Tool for Plain {
    fn name(&self) -> &str {
        self.name
    }

    fn class(&self) -> ActionClass {
        self.class
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["to"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, _args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok(self.answer.to_owned()) })
    }
}

/// The runs, by prompt.
fn driver(desk: &Arc<CheckpointDesk>) -> FakeDriver {
    let mut gateway = ToolGateway::new();
    for tool in [
        Plain {
            name: "fs.read",
            class: ActionClass::ReadLocal,
            answer: "- Call the dentist\n- Draft the Q4 plan\n- Book flights for December",
        },
        Plain {
            name: "fs.write",
            class: ActionClass::WriteLocal,
            answer: "Saved notes.md (412 bytes).",
        },
        Plain {
            name: "email.send",
            class: ActionClass::CommsSend,
            answer: "Sent.",
        },
    ] {
        gateway.register(Arc::new(tool)).expect("register");
    }
    let n = Arc::new(AtomicU64::new(0));
    let ids = Arc::new(move || format!("id-{}", n.fetch_add(1, Ordering::SeqCst) + 1));
    let wait = |s| Duration::from_secs(s);
    FakeDriver::new(FakeScript::default())
        .with_prompt(
            "Plan my week",
            FakeScript::default()
                .wait(wait(2))
                .say("Here's a plan: Monday for the Q4 draft, Wednesday for errands, Friday free.")
                .usage(1_200, 340, 4_400),
        )
        .with_prompt(
            "Tidy my notes",
            FakeScript::default()
                .say("I'll read your notes first.")
                .tool("fs.read", json!({"path": "notes.md"}))
                .wait(wait(3))
                .say(" Grouping them by week.")
                .tool(
                    "fs.write",
                    json!({"path": "notes.md", "text": "## This week\n- Call the dentist"}),
                )
                .say(" Done: notes.md is grouped by week.")
                .usage(3_800, 910, 13_100),
        )
        .with_prompt(
            "Email the summary to Sam",
            FakeScript::default()
                .say("Sending the summary to Sam.")
                .tool(
                    "email.send",
                    json!({"to": "sam@example.test", "body": "The Q4 summary"}),
                )
                .say(" OK, I won't send it.")
                .usage(900, 120, 2_500),
        )
        .with_prompt(
            "Find flights to Lisbon",
            FakeScript::default()
                .say("Looking up flights")
                .wait(wait(4))
                .fail(
                    FailureReason::Provider,
                    "xAI returned 503: the service is overloaded",
                ),
        )
        .with_prompt(
            "Research Lisbon neighborhoods",
            FakeScript::default()
                .say("Reading travel guides")
                .wait(wait(600))
                .say(" never reached"),
        )
        .with_prompt(
            "Send the itinerary to Sam",
            FakeScript::default().say("Sending the itinerary.").tool(
                "email.send",
                json!({"to": "sam@example.test", "body": "The itinerary"}),
            ),
        )
        .with_tools(ToolSetup {
            policy: Policy::new(Profile::default()),
            gateway: Arc::new(gateway),
            desk: Arc::clone(desk),
            ids,
        })
}

async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

async fn idle(sup: &Supervisor) {
    tokio::time::timeout(Duration::from_secs(3600), sup.wait_idle())
        .await
        .expect("idle");
}

fn deploy(sup: &Supervisor, run: &str, agent: &str, prompt: &str) {
    sup.deploy(RunRequest {
        run_id: RunId::from(run),
        agent_id: AgentId::from(agent),
        session_id: SessionId::from(format!("s-{run}").as_str()),
        prompt: prompt.into(),
    })
    .expect("deploy");
}

/// Answers the one open checkpoint, after the person took 12 seconds to read it.
async fn answer(desk: &CheckpointDesk, how: Answer) {
    tokio::time::sleep(Duration::from_secs(12)).await;
    settle().await;
    let open = desk.waiting();
    let [id] = open.as_slice() else {
        panic!("one checkpoint open: {open:?}");
    };
    desk.answer(id, how).expect("answered");
}

fn fixtures_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/fixtures/traces.json")
}

/// Plays the runs and reads them back as the trace viewer does.
async fn play() -> String {
    let dir = std::env::temp_dir().join(format!("corral-story-fixtures-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    let store = Arc::new(Store::open(&dir.join("corral.db")).expect("store"));
    let desk = Arc::new(CheckpointDesk::default());
    let sup = Supervisor::with_budget(
        SupervisorConfig::default(),
        BudgetConfig::default(),
        Arc::new(driver(&desk)),
        store.clone(),
        Arc::new(PausedClock(tokio::time::Instant::now())),
    );

    deploy(&sup, "run-plan", "grok", "Plan my week");
    idle(&sup).await;
    deploy(&sup, "run-tidy", "claude", "Tidy my notes");
    answer(
        &desk,
        Answer::Approve {
            how: Approval::Click,
            scope: Scope::Once,
        },
    )
    .await;
    idle(&sup).await;
    deploy(&sup, "run-email", "claude", "Email the summary to Sam");
    answer(
        &desk,
        Answer::Deny {
            note: Some("Not yet: I'll send it myself.".into()),
        },
    )
    .await;
    idle(&sup).await;
    deploy(&sup, "run-flights", "grok", "Find flights to Lisbon");
    idle(&sup).await;
    deploy(
        &sup,
        "run-research",
        "grok",
        "Research Lisbon neighborhoods",
    );
    tokio::time::sleep(Duration::from_secs(90)).await;
    sup.cancel(&RunId::from("run-research"))
        .await
        .expect("stopped");
    idle(&sup).await;
    deploy(&sup, "run-itinerary", "claude", "Send the itinerary to Sam");
    tokio::time::sleep(Duration::from_secs(5)).await;
    settle().await;

    let traces = Traces::default();
    traces.attach(store);
    let runs = traces.recent_runs().expect("runs");
    let mut by_run = BTreeMap::new();
    for run in &runs {
        let id = run.run_id.clone();
        by_run.insert(id.clone(), traces.run_trace(&id).expect("trace"));
    }
    let text =
        serde_json::to_string_pretty(&json!({ "runs": runs, "traces": by_run })).expect("json");
    sup.cancel(&RunId::from("run-itinerary"))
        .await
        .expect("stopped");
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!("couldn't remove {}: {e}", dir.display());
    }
    format!("{text}\n")
}

#[tokio::test(start_paused = true)]
async fn tp_sb_01_the_trace_fixtures_are_what_the_core_produces() {
    let played = play().await;
    for state in [
        "\"completed\"",
        "\"failed\"",
        "\"cancelled\"",
        "\"running\"",
        "checkpoint",
    ] {
        assert!(played.contains(state), "the fixtures cover {state}");
    }
    let path = fixtures_path();
    if std::env::var_os("CORRAL_WRITE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("fixtures folder");
        std::fs::write(&path, &played).expect("write");
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} can't be read ({e}); write it with CORRAL_WRITE_FIXTURES=1",
            path.display()
        )
    });
    assert!(
        committed == played,
        "the trace fixtures are out of date: run CORRAL_WRITE_FIXTURES=1 cargo test -p \
         corral-desktop --test story_fixtures"
    );
}
