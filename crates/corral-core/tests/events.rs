//! Event v1 schema: TP-EVT-01..03 (`docs/04-test-plan.md`, system design §6).

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use std::path::{Path, PathBuf};

/// The v1 kinds, exactly as listed in `docs/01-system-design.md` §6, in that order.
const SPEC_KINDS: [&str; 20] = [
    "run.queued",
    "run.started",
    "plan.updated",
    "model.request",
    "model.delta",
    "model.response",
    "tool.requested",
    "policy.decided",
    "checkpoint.opened",
    "checkpoint.resolved",
    "tool.started",
    "tool.result",
    "usage.reported",
    "budget.warned",
    "run.paused",
    "run.resumed",
    "run.completed",
    "run.failed",
    "run.cancelled",
    "session.closed",
];

/// One representative body per kind. The match has no wildcard arm, so adding a kind without
/// a sample here is a compile error (AGENTS.md rule 8).
fn sample(kind: EventKind) -> EventBody {
    let call = ToolCallId::from("call-0001");
    match kind {
        EventKind::RunQueued => EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from("grok"),
            session_id: SessionId::from("session-0001"),
            prompt: "Summarize the open pull requests".into(),
        }),
        EventKind::RunStarted => EventBody::RunStarted(RunStarted {
            adapter: "openai_responses".into(),
        }),
        EventKind::PlanUpdated => EventBody::PlanUpdated(PlanUpdated {
            steps: vec![
                PlanStep {
                    title: "Read the PRs".into(),
                    status: StepStatus::Done,
                },
                PlanStep {
                    title: "Rank them".into(),
                    status: StepStatus::InProgress,
                },
            ],
        }),
        EventKind::ModelRequest => EventBody::ModelRequest(ModelRequest {
            provider: "xai".into(),
            model: "example-model".into(),
        }),
        EventKind::ModelDelta => EventBody::ModelDelta(ModelDelta {
            text: "Three PRs are open".into(),
        }),
        EventKind::ModelResponse => EventBody::ModelResponse(ModelResponse {
            text: "Three PRs are open; #32 is safest.".into(),
            finish_reason: Some("stop".into()),
        }),
        EventKind::ToolRequested => EventBody::ToolRequested(ToolRequested {
            tool_call_id: call.clone(),
            tool: "fs.write".into(),
            action_class: ActionClass::WriteLocal,
            args: serde_json::json!({ "path": "notes.md" }),
        }),
        EventKind::PolicyDecided => EventBody::PolicyDecided(PolicyDecided {
            tool_call_id: call.clone(),
            decision: PolicyDecision::Deny {
                reason: "unknown action class".into(),
            },
        }),
        EventKind::CheckpointOpened => EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from("cp-0001"),
            tool_call_id: call.clone(),
            action_class: ActionClass::MoneySpend,
            risk: Risk::Critical,
            title: "Buy a ticket".into(),
            detail: "$42.00 to example.test".into(),
        }),
        EventKind::CheckpointResolved => EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from("cp-0001"),
            resolution: Resolution::Edited {
                args: serde_json::json!({ "amount": 40 }),
            },
        }),
        EventKind::ToolStarted => EventBody::ToolStarted(ToolStarted {
            tool_call_id: call.clone(),
        }),
        EventKind::ToolResult => EventBody::ToolResult(ToolResult {
            tool_call_id: call,
            ok: false,
            output: serde_json::json!({ "error": "denied" }),
        }),
        EventKind::UsageReported => EventBody::UsageReported(UsageReported {
            input_tokens: 10,
            output_tokens: 20,
            cost_micro_usd: 30,
        }),
        EventKind::BudgetWarned => EventBody::BudgetWarned(BudgetWarned {
            scope: BudgetScope::AgentMonth,
            spent_micro_usd: 9_000_000,
            limit_micro_usd: 10_000_000,
        }),
        EventKind::RunPaused => EventBody::RunPaused(RunPaused {
            reason: PauseReason::User,
        }),
        EventKind::RunResumed => EventBody::RunResumed(RunResumed {}),
        EventKind::RunCompleted => EventBody::RunCompleted(RunCompleted { summary: None }),
        EventKind::RunFailed => EventBody::RunFailed(RunFailed {
            reason: FailureReason::BudgetHardCap,
            message: "The run hit its spend cap.".into(),
        }),
        EventKind::RunCancelled => EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::User,
        }),
        EventKind::SessionClosed => EventBody::SessionClosed(SessionClosed {}),
    }
}

fn event(seq: u64, body: EventBody) -> Event {
    Event {
        run_id: RunId::from("run-0001"),
        seq,
        ts: 1_762_761_600_000 + seq,
        span_id: SpanId::from(format!("span-{seq:04}").as_str()),
        parent_span_id: seq.is_multiple_of(2).then(|| SpanId::from("span-0001")),
        body,
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events")
}

#[test]
fn tp_evt_01_kinds_match_the_spec() {
    let ours: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        ours, SPEC_KINDS,
        "EventKind::ALL must list the §6 kinds in order"
    );
    for kind in EventKind::ALL {
        assert_eq!(kind.as_str().parse::<EventKind>(), Ok(kind));
    }
}

#[test]
fn tp_evt_01_every_kind_round_trips_through_json() {
    for (seq, kind) in (1u64..).zip(EventKind::ALL) {
        let original = event(seq, sample(kind));
        assert_eq!(original.kind(), kind);

        let json = original.to_json().expect("serialize");
        let back = Event::from_json(&json).expect("deserialize");
        assert_eq!(
            back,
            original,
            "{} changed in a JSON round trip",
            kind.as_str()
        );
    }
}

#[test]
fn tp_evt_02_golden_fixtures_reserialize_byte_identically() {
    let mut seen = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir()).expect("read fixtures dir") {
        let path = entry.expect("fixture entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let event = Event::from_json(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            event.to_json_pretty().expect("serialize"),
            text,
            "{} doesn't re-serialize byte for byte",
            path.display()
        );
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("file name");
        assert_eq!(
            stem,
            event.kind().as_str(),
            "fixture is named after its kind"
        );
        seen.push(event.kind());
    }
    for kind in EventKind::ALL {
        assert!(
            seen.contains(&kind),
            "no golden fixture for {}",
            kind.as_str()
        );
    }
}

#[test]
fn tp_evt_03_unknown_kind_is_a_typed_error() {
    let json = r#"{"run_id":"r","seq":1,"ts":0,"span_id":"s","kind":"run.exploded","payload":{}}"#;
    assert_eq!(
        Event::from_json(json),
        Err(EventError::UnknownKind("run.exploded".into()))
    );
    assert_eq!(
        "run.exploded".parse::<EventKind>(),
        Err(EventError::UnknownKind("run.exploded".into()))
    );
}

#[test]
fn tp_evt_03_missing_kind_or_bad_payload_is_malformed() {
    let no_kind = r#"{"run_id":"r","seq":1,"ts":0,"span_id":"s","payload":{}}"#;
    assert!(matches!(
        Event::from_json(no_kind),
        Err(EventError::Malformed(_))
    ));

    let bad_payload = r#"{"run_id":"r","seq":1,"ts":0,"span_id":"s","kind":"run.paused","payload":{"reason":"nap"}}"#;
    assert!(matches!(
        Event::from_json(bad_payload),
        Err(EventError::Malformed(_))
    ));

    let not_json = "{ not json";
    assert!(matches!(
        Event::from_json(not_json),
        Err(EventError::Malformed(_))
    ));
}

#[test]
fn manual_clock_is_deterministic() {
    let clock = ManualClock::at(1_000);
    assert_eq!(clock.now_ms(), 1_000);
    clock.advance(250);
    assert_eq!(clock.now_ms(), 1_250);
}
