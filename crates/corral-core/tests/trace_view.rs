//! The trace viewer's span tree, projected from one run's events (M4 item 5, docs/01 §7,
//! TP-TV-01..09). The log is the only source (ADR-002): a run span, a span per model call and per
//! tool call under it, and a span per checkpoint under its tool call, each with its start and
//! end, a status, and attributes for the side panel. Model text and arguments are agent text:
//! cleaned and capped.

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::trace_view::{MAX_ATTRIBUTE_CHARS, Span, SpanKind, SpanStatus, trace_view};
use serde_json::json;

fn run(bodies: Vec<(u64, EventBody)>) -> Vec<Event> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, (ts, body))| Event {
            run_id: RunId::from("r1"),
            seq: i as u64 + 1,
            ts,
            span_id: SpanId::from("r1:invoke_agent"),
            parent_span_id: None,
            body,
        })
        .collect()
}

fn queued() -> EventBody {
    EventBody::RunQueued(RunQueued {
        agent_id: AgentId::from("claude"),
        session_id: SessionId::from("s1"),
        prompt: "Tidy my notes".into(),
    })
}

fn started() -> EventBody {
    EventBody::RunStarted(RunStarted {
        adapter: "anthropic.messages".into(),
    })
}

fn request() -> EventBody {
    EventBody::ModelRequest(ModelRequest {
        provider: "anthropic".into(),
        model: "claude-sonnet-5-5".into(),
    })
}

fn response(text: &str) -> EventBody {
    EventBody::ModelResponse(ModelResponse {
        text: text.into(),
        finish_reason: Some("end_turn".into()),
    })
}

fn usage(input: u64, output: u64, cost: u64) -> EventBody {
    EventBody::UsageReported(UsageReported {
        input_tokens: input,
        output_tokens: output,
        cost_micro_usd: cost,
    })
}

fn tool_requested(id: &str, tool: &str, class: ActionClass) -> EventBody {
    EventBody::ToolRequested(ToolRequested {
        tool_call_id: ToolCallId::from(id),
        tool: tool.into(),
        action_class: class,
        args: json!({"path": "notes.md"}),
    })
}

fn decided(id: &str, decision: PolicyDecision) -> EventBody {
    EventBody::PolicyDecided(PolicyDecided {
        tool_call_id: ToolCallId::from(id),
        decision,
    })
}

fn tool_started(id: &str) -> EventBody {
    EventBody::ToolStarted(ToolStarted {
        tool_call_id: ToolCallId::from(id),
    })
}

fn tool_result(id: &str, ok: bool, output: serde_json::Value) -> EventBody {
    EventBody::ToolResult(ToolResult {
        tool_call_id: ToolCallId::from(id),
        ok,
        output,
    })
}

fn completed() -> EventBody {
    EventBody::RunCompleted(RunCompleted { summary: None })
}

fn attr<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
    span.attributes
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.value.as_str())
}

fn span<'a>(spans: &'a [Span], id: &str) -> &'a Span {
    spans.iter().find(|s| s.id == id).unwrap_or_else(|| {
        panic!(
            "no span {id}: {:?}",
            spans.iter().map(|s| &s.id).collect::<Vec<_>>()
        )
    })
}

// ---- TP-TV-01: the run and its model calls ---------------------------------------------------

#[test]
fn tp_tv_01_a_run_with_one_answer_is_a_run_span_over_a_model_span() {
    let view = trace_view(&run(vec![
        (1_000, queued()),
        (1_010, started()),
        (1_020, request()),
        (1_900, response("Done.")),
        (1_901, usage(120, 30, 540)),
        (1_902, completed()),
    ]));
    assert_eq!(view.run_id, "r1");
    assert_eq!((view.start_ms, view.end_ms), (1_000, Some(1_902)));
    let ids: Vec<&str> = view.spans.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["run", "model-1"]);

    let root = span(&view.spans, "run");
    assert_eq!(root.parent, None);
    assert_eq!(root.kind, SpanKind::Run);
    assert_eq!(root.name, "invoke_agent claude");
    assert_eq!((root.start_ms, root.end_ms), (1_000, Some(1_902)));
    assert_eq!(root.status, SpanStatus::Ok);
    assert_eq!(attr(root, "prompt"), Some("Tidy my notes"));
    assert_eq!(attr(root, "adapter"), Some("anthropic.messages"));
    assert_eq!(attr(root, "input tokens"), Some("120"));
    assert_eq!(attr(root, "output tokens"), Some("30"));
    assert_eq!(attr(root, "cost"), Some("$0.0006"));

    let model = span(&view.spans, "model-1");
    assert_eq!(model.parent.as_deref(), Some("run"));
    assert_eq!(model.kind, SpanKind::Model);
    assert_eq!(model.name, "chat claude-sonnet-5-5");
    assert_eq!((model.start_ms, model.end_ms), (1_020, Some(1_900)));
    assert_eq!(model.status, SpanStatus::Ok);
    assert_eq!(attr(model, "provider"), Some("anthropic"));
    assert_eq!(attr(model, "finish reason"), Some("end_turn"));
    assert_eq!(attr(model, "response"), Some("Done."));
    assert_eq!(attr(model, "input tokens"), Some("120"));
    assert_eq!(attr(model, "cost"), Some("$0.0006"));
}

#[test]
fn tp_tv_01_each_turn_is_its_own_model_span_with_its_own_usage() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (2, started()),
        (3, request()),
        (4, response("")),
        (5, usage(10, 2, 100)),
        (6, request()),
        (7, response("Second.")),
        (8, usage(20, 4, 200)),
        (9, completed()),
    ]));
    let first = span(&view.spans, "model-1");
    let second = span(&view.spans, "model-2");
    assert_eq!(attr(first, "cost"), Some("$0.0001"));
    assert_eq!(attr(second, "cost"), Some("$0.0002"));
    assert_eq!(attr(second, "response"), Some("Second."));
    assert_eq!(attr(span(&view.spans, "run"), "cost"), Some("$0.0003"));
}

// ---- TP-TV-02..04: tool calls and checkpoints ------------------------------------------------

#[test]
fn tp_tv_02_an_allowed_tool_call_is_a_span_beside_the_model_calls() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (2, started()),
        (3, request()),
        (4, response("")),
        (5, tool_requested("c1", "fs.read", ActionClass::ReadLocal)),
        (6, decided("c1", PolicyDecision::Allow)),
        (7, tool_started("c1")),
        (9, tool_result("c1", true, json!("# Notes"))),
        (10, completed()),
    ]));
    let tool = span(&view.spans, "tool-c1");
    assert_eq!(tool.parent.as_deref(), Some("run"));
    assert_eq!(tool.kind, SpanKind::Tool);
    assert_eq!(tool.name, "execute_tool fs.read");
    assert_eq!((tool.start_ms, tool.end_ms), (5, Some(9)));
    assert_eq!(tool.status, SpanStatus::Ok);
    assert_eq!(attr(tool, "class"), Some("read.local"));
    assert_eq!(attr(tool, "arguments"), Some(r#"{"path":"notes.md"}"#));
    assert_eq!(attr(tool, "decision"), Some("allow"));
    assert_eq!(attr(tool, "result"), Some(r##""# Notes""##));

    let failed = trace_view(&run(vec![
        (1, queued()),
        (5, tool_requested("c2", "fs.read", ActionClass::ReadLocal)),
        (6, decided("c2", PolicyDecision::Allow)),
        (9, tool_result("c2", false, json!({"error": "not found"}))),
    ]));
    assert_eq!(span(&failed.spans, "tool-c2").status, SpanStatus::Error);
}

#[test]
fn tp_tv_03_a_checkpoint_is_a_span_under_its_tool_call() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (5, tool_requested("c1", "fs.write", ActionClass::WriteLocal)),
        (
            6,
            decided(
                "c1",
                PolicyDecision::Ask {
                    risk: Risk::High,
                    reason: "writes a file".into(),
                },
            ),
        ),
        (
            7,
            EventBody::CheckpointOpened(CheckpointOpened {
                checkpoint_id: CheckpointId::from("k1"),
                tool_call_id: ToolCallId::from("c1"),
                action_class: ActionClass::WriteLocal,
                risk: Risk::High,
                title: "Write notes.md".into(),
                detail: "Replaces notes.md".into(),
            }),
        ),
        (
            40,
            EventBody::CheckpointResolved(CheckpointResolved {
                checkpoint_id: CheckpointId::from("k1"),
                resolution: Resolution::Denied {
                    note: Some("not yet".into()),
                },
            }),
        ),
        (41, tool_result("c1", false, json!("denied"))),
        (42, completed()),
    ]));
    let tool = span(&view.spans, "tool-c1");
    assert_eq!(attr(tool, "decision"), Some("ask (high): writes a file"));
    let checkpoint = span(&view.spans, "checkpoint-k1");
    assert_eq!(checkpoint.parent.as_deref(), Some("tool-c1"));
    assert_eq!(checkpoint.kind, SpanKind::Checkpoint);
    assert_eq!(checkpoint.name, "checkpoint: Write notes.md");
    assert_eq!((checkpoint.start_ms, checkpoint.end_ms), (7, Some(40)));
    assert_eq!(checkpoint.status, SpanStatus::Denied);
    assert_eq!(attr(checkpoint, "risk"), Some("high"));
    assert_eq!(attr(checkpoint, "detail"), Some("Replaces notes.md"));
    assert_eq!(attr(checkpoint, "resolution"), Some("denied: not yet"));
    let ids: Vec<&str> = view.spans.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        ["run", "tool-c1", "checkpoint-k1"],
        "in the order they began"
    );

    for (resolution, status, text) in [
        (Resolution::Approved, SpanStatus::Ok, "approved"),
        (
            Resolution::Edited {
                args: json!({"path": "b.md"}),
            },
            SpanStatus::Ok,
            "edited",
        ),
        (
            Resolution::AllowedForRun,
            SpanStatus::Ok,
            "allowed for the run",
        ),
        (Resolution::Expired, SpanStatus::Cancelled, "expired"),
        (
            Resolution::Denied { note: None },
            SpanStatus::Denied,
            "denied",
        ),
    ] {
        let view = trace_view(&run(vec![
            (1, queued()),
            (2, tool_requested("c1", "fs.write", ActionClass::WriteLocal)),
            (
                3,
                EventBody::CheckpointOpened(CheckpointOpened {
                    checkpoint_id: CheckpointId::from("k1"),
                    tool_call_id: ToolCallId::from("c1"),
                    action_class: ActionClass::WriteLocal,
                    risk: Risk::Normal,
                    title: "Write".into(),
                    detail: String::new(),
                }),
            ),
            (
                4,
                EventBody::CheckpointResolved(CheckpointResolved {
                    checkpoint_id: CheckpointId::from("k1"),
                    resolution,
                }),
            ),
        ]));
        let checkpoint = span(&view.spans, "checkpoint-k1");
        assert_eq!(checkpoint.status, status, "{text}");
        assert_eq!(attr(checkpoint, "resolution"), Some(text));
    }
}

#[test]
fn tp_tv_04_a_denied_call_ends_where_the_policy_said_no() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (5, tool_requested("c1", "shell", ActionClass::ExecShell)),
        (
            6,
            decided(
                "c1",
                PolicyDecision::Deny {
                    reason: "no shell".into(),
                },
            ),
        ),
        (7, completed()),
    ]));
    let tool = span(&view.spans, "tool-c1");
    assert_eq!(tool.status, SpanStatus::Denied);
    assert_eq!(tool.end_ms, Some(6));
    assert_eq!(attr(tool, "decision"), Some("deny: no shell"));
}

// ---- TP-TV-05..06: how runs end ------------------------------------------------------------

#[test]
fn tp_tv_05_what_was_open_when_a_run_failed_or_stopped_ends_with_it() {
    let failed = trace_view(&run(vec![
        (1, queued()),
        (2, request()),
        (
            9,
            EventBody::RunFailed(RunFailed {
                reason: FailureReason::Provider,
                message: "Anthropic is overloaded.".into(),
            }),
        ),
    ]));
    let model = span(&failed.spans, "model-1");
    assert_eq!((model.status, model.end_ms), (SpanStatus::Error, Some(9)));
    let root = span(&failed.spans, "run");
    assert_eq!(root.status, SpanStatus::Error);
    assert_eq!(attr(root, "error"), Some("Anthropic is overloaded."));

    let stopped = trace_view(&run(vec![
        (1, queued()),
        (2, tool_requested("c1", "fs.read", ActionClass::ReadLocal)),
        (
            9,
            EventBody::RunCancelled(RunCancelled {
                by: CancelledBy::User,
            }),
        ),
    ]));
    let tool = span(&stopped.spans, "tool-c1");
    assert_eq!((tool.status, tool.end_ms), (SpanStatus::Cancelled, Some(9)));
    assert_eq!(span(&stopped.spans, "run").status, SpanStatus::Cancelled);
}

#[test]
fn tp_tv_06_a_run_still_going_has_open_spans() {
    let view = trace_view(&run(vec![(1, queued()), (2, started()), (3, request())]));
    assert_eq!(view.end_ms, None);
    for id in ["run", "model-1"] {
        let open = span(&view.spans, id);
        assert_eq!(
            (open.status, open.end_ms),
            (SpanStatus::Running, None),
            "{id}"
        );
    }
    assert!(trace_view(&[]).spans.is_empty(), "no events, no spans");
}

// ---- TP-TV-07: agent text --------------------------------------------------------------------

#[test]
fn tp_tv_07_model_text_and_arguments_are_cleaned_and_capped() {
    let long = "x".repeat(MAX_ATTRIBUTE_CHARS + 50);
    let view = trace_view(&run(vec![
        (1, queued()),
        (2, request()),
        (3, response("evil\u{202e}gnp.exe\u{7} text")),
        (4, request()),
        (5, response(&long)),
    ]));
    let cleaned = attr(span(&view.spans, "model-1"), "response").expect("response");
    assert!(
        !cleaned.contains('\u{202e}') && !cleaned.contains('\u{7}'),
        "{cleaned:?}"
    );
    let capped = attr(span(&view.spans, "model-2"), "response").expect("response");
    assert!(
        capped.chars().count() <= MAX_ATTRIBUTE_CHARS,
        "{}",
        capped.chars().count()
    );
    assert_eq!(MAX_ATTRIBUTE_CHARS, 2_000);
}

// ---- TP-TV-08..09: marks, and odd logs ------------------------------------------------------

#[test]
fn tp_tv_08_budget_warnings_pauses_and_plans_are_marks_on_the_run() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (
            5,
            EventBody::BudgetWarned(BudgetWarned {
                scope: BudgetScope::Run,
                spent_micro_usd: 100_000,
                limit_micro_usd: 250_000,
            }),
        ),
        (
            6,
            EventBody::RunPaused(RunPaused {
                reason: PauseReason::Budget,
            }),
        ),
        (7, EventBody::RunResumed(RunResumed {})),
    ]));
    let marks: Vec<(&str, SpanKind, Option<u64>)> = view
        .spans
        .iter()
        .filter(|s| s.kind == SpanKind::Mark)
        .map(|s| (s.name.as_str(), s.kind, s.end_ms))
        .collect();
    assert_eq!(
        marks,
        [
            (
                "budget warning: $0.10 of $0.25 for the run",
                SpanKind::Mark,
                Some(5)
            ),
            ("paused: budget", SpanKind::Mark, Some(6)),
            ("resumed", SpanKind::Mark, Some(7)),
        ]
    );
    assert!(
        view.spans
            .iter()
            .filter(|s| s.kind == SpanKind::Mark)
            .all(|s| s.parent.as_deref() == Some("run") && s.status == SpanStatus::Ok)
    );
}

#[test]
fn tp_tv_09_a_result_with_no_request_still_shows() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (4, tool_result("ghost", true, json!("x"))),
    ]));
    let tool = span(&view.spans, "tool-ghost");
    assert_eq!(tool.name, "execute_tool ?");
    assert_eq!((tool.start_ms, tool.end_ms), (4, Some(4)));
}

#[test]
fn tp_tv_09_the_view_is_sent_as_camel_case_json() {
    let view = trace_view(&run(vec![(1, queued()), (2, completed())]));
    let value = serde_json::to_value(&view).expect("json");
    assert_eq!(value["runId"], "r1");
    assert_eq!(value["startMs"], 1);
    assert_eq!(value["endMs"], 2);
    assert_eq!(value["spans"][0]["kind"], "run");
    assert_eq!(value["spans"][0]["status"], "ok");
    assert_eq!(value["spans"][0]["parent"], serde_json::Value::Null);
    assert_eq!(
        value["spans"][0]["attributes"][0],
        json!({"key": "prompt", "value": "Tidy my notes"})
    );
}

#[test]
fn tp_tv_08_a_plan_update_is_a_mark_with_its_steps() {
    let step = |title: &str, status: StepStatus| PlanStep {
        title: title.into(),
        status,
    };
    let view = trace_view(&run(vec![
        (1, queued()),
        (
            3,
            EventBody::PlanUpdated(PlanUpdated {
                steps: vec![
                    step("Read notes", StepStatus::Done),
                    step("Sort them", StepStatus::InProgress),
                    step("Write summary", StepStatus::Pending),
                    step("Email it", StepStatus::Skipped),
                ],
            }),
        ),
    ]));
    let plan = span(&view.spans, "mark-1");
    assert_eq!(plan.name, "plan: 1 of 4 done");
    assert_eq!(
        attr(plan, "steps"),
        Some(
            "Read notes (done)\nSort them (in progress)\nWrite summary (pending)\nEmail it (skipped)"
        )
    );
}

#[test]
fn tp_tv_08_each_mark_has_its_own_id() {
    let view = trace_view(&run(vec![
        (1, queued()),
        (
            2,
            EventBody::RunPaused(RunPaused {
                reason: PauseReason::User,
            }),
        ),
        (3, EventBody::RunResumed(RunResumed {})),
        (4, EventBody::RunResumed(RunResumed {})),
    ]));
    let ids: Vec<&str> = view.spans.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["run", "mark-1", "mark-2", "mark-3"]);
    assert_eq!(span(&view.spans, "mark-1").name, "paused: by you");
}
