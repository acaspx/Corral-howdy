//! `plan_view` (M4 item 3, docs/07 §8.3, TP-TRACE-01): a run's subtasks for the hover card and the
//! trace tree. The agent's published plan when there is one (`plan.updated`), otherwise the run's
//! tool calls. Swift never parses provider output; it shows this.

use corral_core::agent_text::AgentText;
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::plan_view::{
    MAX_SUBTASKS, PlanSource, PlanView, SUBTASK_TITLE_MAX_CHARS, Subtask, SubtaskStatus,
};
use corral_core::run_card::RunCards;
use serde_json::json;

fn event(run: &str, seq: u64, body: EventBody) -> Event {
    Event {
        run_id: RunId::from(run),
        seq,
        ts: 1_000 + seq,
        span_id: SpanId::from("s"),
        parent_span_id: None,
        body,
    }
}

fn queued(agent: &str) -> EventBody {
    EventBody::RunQueued(RunQueued {
        agent_id: AgentId::from(agent),
        session_id: SessionId::from("s1"),
        prompt: "Tidy my notes".into(),
    })
}

fn started() -> EventBody {
    EventBody::RunStarted(RunStarted {
        adapter: "anthropic.messages".into(),
    })
}

fn plan(steps: &[(&str, StepStatus)]) -> EventBody {
    EventBody::PlanUpdated(PlanUpdated {
        steps: steps
            .iter()
            .map(|(title, status)| PlanStep {
                title: (*title).into(),
                status: *status,
            })
            .collect(),
    })
}

fn requested(id: &str, tool: &str) -> EventBody {
    EventBody::ToolRequested(ToolRequested {
        tool_call_id: ToolCallId::from(id),
        tool: tool.into(),
        action_class: ActionClass::ReadLocal,
        args: json!({"path": "notes.md"}),
    })
}

fn result(id: &str, ok: bool) -> EventBody {
    EventBody::ToolResult(ToolResult {
        tool_call_id: ToolCallId::from(id),
        ok,
        output: json!("…"),
    })
}

fn denied_by_policy(id: &str) -> EventBody {
    EventBody::PolicyDecided(PolicyDecided {
        tool_call_id: ToolCallId::from(id),
        decision: PolicyDecision::Deny {
            reason: "not allowed".into(),
        },
    })
}

fn view(bodies: Vec<EventBody>) -> PlanView {
    let mut view = PlanView::default();
    for body in &bodies {
        view.observe(body);
    }
    view
}

fn steps(view: &PlanView) -> Vec<(&str, SubtaskStatus)> {
    view.subtasks()
        .iter()
        .map(|s| (s.title.as_str(), s.status))
        .collect()
}

#[test]
fn tp_trace_01_a_published_plan_is_the_subtasks_and_each_update_replaces_it() {
    let v = view(vec![plan(&[
        ("Read the notes", StepStatus::Done),
        ("Group by topic", StepStatus::InProgress),
        ("Write the summary", StepStatus::Pending),
        ("Email it", StepStatus::Skipped),
    ])]);
    assert_eq!(v.source(), Some(PlanSource::Plan));
    assert_eq!(
        steps(&v),
        [
            ("Read the notes", SubtaskStatus::Done),
            ("Group by topic", SubtaskStatus::Current),
            ("Write the summary", SubtaskStatus::Pending),
            ("Email it", SubtaskStatus::Skipped)
        ]
    );
    assert_eq!(
        v.current().map(|(i, s)| (i, s.title.as_str())),
        Some((1, "Group by topic"))
    );
    assert_eq!(v.total(), 4);

    let v = view(vec![
        plan(&[
            ("One", StepStatus::InProgress),
            ("Two", StepStatus::Pending),
        ]),
        plan(&[("One", StepStatus::Done), ("Two", StepStatus::InProgress)]),
    ]);
    assert_eq!(
        steps(&v),
        [
            ("One", SubtaskStatus::Done),
            ("Two", SubtaskStatus::Current)
        ]
    );
}

#[test]
fn tp_trace_01_without_a_plan_the_tool_calls_are_the_subtasks() {
    let v = view(vec![
        started(),
        requested("t1", "fs.read"),
        result("t1", true),
        requested("t2", "web_search"),
    ]);
    assert_eq!(v.source(), Some(PlanSource::Tools));
    assert_eq!(
        steps(&v),
        [
            ("fs.read", SubtaskStatus::Done),
            ("web_search", SubtaskStatus::Current)
        ]
    );
    assert_eq!(v.current().map(|(i, _)| i), Some(1));

    let v = view(vec![
        requested("t1", "fs.write"),
        result("t1", false),
        requested("t2", "shell"),
        denied_by_policy("t2"),
    ]);
    assert_eq!(
        steps(&v),
        [
            ("fs.write", SubtaskStatus::Failed),
            ("shell", SubtaskStatus::Skipped)
        ]
    );
    assert_eq!(v.current(), None);
}

#[test]
fn tp_trace_01_a_plan_replaces_the_tool_calls_once_published() {
    let v = view(vec![
        requested("t1", "fs.read"),
        plan(&[("Read", StepStatus::InProgress)]),
        result("t1", true),
        requested("t2", "fs.read"),
    ]);
    assert_eq!(v.source(), Some(PlanSource::Plan));
    assert_eq!(
        steps(&v),
        [("Read", SubtaskStatus::Current)],
        "tools no longer add steps"
    );
}

#[test]
fn tp_trace_01_a_denied_checkpoint_skips_its_tool() {
    let v = view(vec![
        requested("t1", "fs.delete"),
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from("c1"),
            tool_call_id: ToolCallId::from("t1"),
            action_class: ActionClass::DeleteLocal,
            risk: Risk::High,
            title: "Delete notes.md".into(),
            detail: "notes.md".into(),
        }),
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from("c1"),
            resolution: Resolution::Denied { note: None },
        }),
    ]);
    assert_eq!(steps(&v), [("fs.delete", SubtaskStatus::Skipped)]);
}

#[test]
fn tp_trace_01_an_expired_checkpoint_skips_its_tool_and_an_approved_one_runs() {
    let opened = |c: &str, t: &str| {
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from(c),
            tool_call_id: ToolCallId::from(t),
            action_class: ActionClass::WriteLocal,
            risk: Risk::Normal,
            title: "Write".into(),
            detail: "notes.md".into(),
        })
    };
    let resolved = |c: &str, resolution: Resolution| {
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from(c),
            resolution,
        })
    };
    let v = view(vec![
        requested("t1", "fs.write"),
        opened("c1", "t1"),
        resolved("c1", Resolution::Expired),
        requested("t2", "fs.write"),
        opened("c2", "t2"),
        resolved("c2", Resolution::AllowedForRun),
        requested("t3", "fs.write"),
        resolved("c9", Resolution::Denied { note: None }),
    ]);
    assert_eq!(
        steps(&v),
        [
            ("fs.write", SubtaskStatus::Skipped),
            ("fs.write", SubtaskStatus::Current),
            ("fs.write", SubtaskStatus::Current)
        ],
        "an unknown checkpoint changes nothing"
    );
    assert_eq!(
        v.current().map(|(i, _)| i),
        Some(2),
        "the latest running tool"
    );
}

#[test]
fn tp_trace_01_no_plan_and_no_tools_is_an_empty_view() {
    let v = view(vec![started()]);
    assert_eq!(v.source(), None);
    assert!(v.subtasks().is_empty());
    assert_eq!((v.current(), v.total()), (None, 0));
}

#[test]
fn tp_trace_01_titles_are_agent_text_and_the_list_is_bounded() {
    let long = format!("{}\u{202e}evil", "x".repeat(500));
    let v = view(vec![plan(&[(long.as_str(), StepStatus::InProgress)])]);
    let title = &v.subtasks()[0].title;
    assert_eq!(*title, AgentText::new(&long, SUBTASK_TITLE_MAX_CHARS));
    assert!(!title.as_str().contains('\u{202e}'));

    let many: Vec<(String, StepStatus)> = (0..MAX_SUBTASKS + 20)
        .map(|i| (format!("Step {i}"), StepStatus::Pending))
        .collect();
    let refs: Vec<(&str, StepStatus)> = many.iter().map(|(t, s)| (t.as_str(), *s)).collect();
    assert_eq!(
        view(vec![plan(&refs)]).total(),
        MAX_SUBTASKS,
        "a plan is capped"
    );

    let mut tools = PlanView::default();
    for i in 0..MAX_SUBTASKS + 20 {
        tools.observe(&requested(&format!("t{i}"), &format!("tool{i}")));
        tools.observe(&result(&format!("t{i}"), true));
    }
    assert_eq!(tools.total(), MAX_SUBTASKS);
    assert_eq!(
        tools
            .subtasks()
            .last()
            .map(|s: &Subtask| s.title.as_str().to_owned()),
        Some(format!("tool{}", MAX_SUBTASKS + 19)),
        "the oldest tool calls go first"
    );
}

#[test]
fn tp_hover_03_the_cards_plan_follows_the_agents_latest_run_and_counts_as_a_change() {
    let mut cards = RunCards::default();
    let agent = AgentId::from("claude");
    assert!(cards.plan(&agent).is_none());
    cards
        .observe(&event("r1", 1, queued("claude")))
        .expect("queued");
    cards.observe(&event("r1", 2, started())).expect("started");
    assert_eq!(cards.plan(&agent).map(PlanView::total), Some(0));
    assert!(
        cards
            .observe(&event("r1", 3, plan(&[("Read", StepStatus::InProgress)])))
            .expect("plan"),
        "a new plan is a change to republish"
    );
    assert!(
        cards
            .observe(&event("r1", 4, requested("t1", "fs.read")))
            .is_ok()
    );
    assert_eq!(cards.plan(&agent).map(PlanView::total), Some(1));

    cards
        .observe(&event(
            "r1",
            5,
            EventBody::RunCompleted(RunCompleted { summary: None }),
        ))
        .expect("done");
    cards
        .observe(&event("r2", 1, queued("claude")))
        .expect("next run");
    assert_eq!(
        cards.plan(&agent).map(PlanView::total),
        Some(0),
        "a new run starts empty"
    );
}
