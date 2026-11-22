//! The trace tree's columns (M4 item 3, docs/07 §8.3, TP-TRACE-02, TP-TRACE-06): which runs get a
//! column above the corral, in what order, and what each shows. The core decides; Swift draws.

use corral_core::agent_text::{AgentText, PROMPT_MAX_CHARS};
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::run_card::{CardStatus, RunCards};
use corral_core::trace_tree::{MAX_COLUMNS, MAX_PEEK, trace_tree};
use serde_json::json;

struct Log {
    cards: RunCards,
    ts: u64,
}

impl Log {
    fn new() -> Self {
        Self {
            cards: RunCards::default(),
            ts: 1_000,
        }
    }

    fn add(&mut self, run: &str, body: EventBody) {
        self.ts += 10;
        let event = Event {
            run_id: RunId::from(run),
            seq: self.ts,
            ts: self.ts,
            span_id: SpanId::from("s"),
            parent_span_id: None,
            body,
        };
        self.cards.observe(&event).expect("legal");
    }

    /// Queues and starts `agent`'s run `run` with `prompt`.
    fn start(&mut self, run: &str, agent: &str, prompt: &str) {
        self.add(
            run,
            EventBody::RunQueued(RunQueued {
                agent_id: AgentId::from(agent),
                session_id: SessionId::from(format!("s-{run}").as_str()),
                prompt: prompt.into(),
            }),
        );
        self.add(
            run,
            EventBody::RunStarted(RunStarted {
                adapter: "fake".into(),
            }),
        );
    }

    fn wait(&mut self, run: &str) {
        self.add(
            run,
            EventBody::ToolRequested(ToolRequested {
                tool_call_id: ToolCallId::from(format!("t-{run}").as_str()),
                tool: "email.send".into(),
                action_class: ActionClass::CommsSend,
                args: json!({}),
            }),
        );
        self.add(
            run,
            EventBody::CheckpointOpened(CheckpointOpened {
                checkpoint_id: CheckpointId::from(format!("c-{run}").as_str()),
                tool_call_id: ToolCallId::from(format!("t-{run}").as_str()),
                action_class: ActionClass::CommsSend,
                risk: Risk::High,
                title: "Sending an email".into(),
                detail: "to me".into(),
            }),
        );
    }

    fn finish(&mut self, run: &str) {
        self.add(run, EventBody::RunCompleted(RunCompleted { summary: None }));
    }
}

fn ids(list: &[&str]) -> Vec<AgentId> {
    list.iter().map(|id| AgentId::from(*id)).collect()
}

#[test]
fn tp_trace_02_at_most_three_columns_needs_you_first_then_the_newest() {
    let mut log = Log::new();
    log.start("r1", "waiting", "Email the report");
    log.start("r2", "old", "Old task");
    log.start("r3", "mid", "Middle task");
    log.start("r4", "new", "New task");
    log.start("r5", "newest", "Newest task");
    log.wait("r1");
    let tree = trace_tree(
        &log.cards,
        &ids(&["old", "mid", "new", "newest", "waiting", "idle"]),
    );
    let order: Vec<(&str, CardStatus)> = tree
        .columns
        .iter()
        .map(|c| (c.agent.as_str(), c.status))
        .collect();
    assert_eq!(
        order,
        [
            ("waiting", CardStatus::NeedsYou),
            ("newest", CardStatus::Working),
            ("new", CardStatus::Working)
        ]
    );
    assert_eq!(tree.columns.len(), MAX_COLUMNS);
    assert_eq!(tree.more, 2, "+2 more running");
}

#[test]
fn tp_trace_06_finished_runs_have_no_column_and_nothing_running_is_no_tree() {
    let mut log = Log::new();
    let tree = trace_tree(&log.cards, &ids(&["grok"]));
    assert!(tree.columns.is_empty() && tree.more == 0);
    log.start("r1", "grok", "Plan the trip");
    assert_eq!(trace_tree(&log.cards, &ids(&["grok"])).columns.len(), 1);
    log.finish("r1");
    let tree = trace_tree(&log.cards, &ids(&["grok"]));
    assert!(
        tree.columns.is_empty(),
        "done folds the column into the drop"
    );
    assert_eq!(tree.more, 0);
}

#[test]
fn tp_trace_02_a_column_shows_the_prompt_the_current_subtask_and_what_peeks_below() {
    let mut log = Log::new();
    log.start("r1", "claude", "Tidy my notes");
    log.add(
        "r1",
        EventBody::PlanUpdated(PlanUpdated {
            steps: ["Read", "Group", "Summarize", "Email", "File"]
                .iter()
                .enumerate()
                .map(|(i, title)| PlanStep {
                    title: (*title).into(),
                    status: match i {
                        0 => StepStatus::Done,
                        1 => StepStatus::InProgress,
                        _ => StepStatus::Pending,
                    },
                })
                .collect(),
        }),
    );
    let tree = trace_tree(&log.cards, &ids(&["claude"]));
    let column = &tree.columns[0];
    assert_eq!(
        column.prompt,
        AgentText::new("Tidy my notes", PROMPT_MAX_CHARS)
    );
    let now = column.subtask.as_ref().expect("a current subtask");
    assert_eq!((now.step, now.total, now.title.as_str()), (2, 5, "Group"));
    let peek: Vec<&str> = column.next.iter().map(AgentText::as_str).collect();
    assert_eq!(peek, ["Summarize", "Email"], "at most two peek out");
    assert_eq!(column.next.len(), MAX_PEEK);
}

#[test]
fn tp_trace_02_a_run_with_no_subtasks_yet_has_no_stack() {
    let mut log = Log::new();
    log.start("r1", "grok", "Plan the trip");
    let tree = trace_tree(&log.cards, &ids(&["grok"]));
    assert!(tree.columns[0].subtask.is_none());
    assert!(tree.columns[0].next.is_empty());
}

#[test]
fn tp_trace_02_an_agent_listed_twice_gets_one_column() {
    let mut log = Log::new();
    log.start("r1", "grok", "Plan the trip");
    let tree = trace_tree(&log.cards, &ids(&["grok", "grok"]));
    assert_eq!(tree.columns.len(), 1);
}

#[test]
fn tp_trace_02_only_what_comes_after_the_current_subtask_peeks_out() {
    let mut log = Log::new();
    log.start("r1", "claude", "Tidy my notes");
    log.add(
        "r1",
        EventBody::PlanUpdated(PlanUpdated {
            steps: [
                ("Skipped ahead", StepStatus::Pending),
                ("Group", StepStatus::InProgress),
                ("Summarize", StepStatus::Pending),
            ]
            .iter()
            .map(|(title, status)| PlanStep {
                title: (*title).into(),
                status: *status,
            })
            .collect(),
        }),
    );
    let tree = trace_tree(&log.cards, &ids(&["claude"]));
    let peek: Vec<&str> = tree.columns[0].next.iter().map(AgentText::as_str).collect();
    assert_eq!(peek, ["Summarize"]);
}
