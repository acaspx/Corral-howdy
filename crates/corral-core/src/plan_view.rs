//! A run's subtasks (M4 item 3, docs/07 §8.3, TP-TRACE-01): what the hover card's progress
//! segments and "Now:" line, and the trace tree's subtask stack, show.
//!
//! - **The agent's plan** when it publishes one (`plan.updated`: ACP plan updates, a native
//!   agent's plan tool). Each update is the whole plan and replaces the last.
//! - **Otherwise the run's tool calls**, in order: current from `tool.requested`, then done or
//!   failed from `tool.result`, or skipped when the policy or a checkpoint denies it (or the
//!   checkpoint expires). Once a plan
//!   is published, tool calls stop adding steps.
//!
//! Titles are agent text (security model T20): cleaned and capped ([`AgentText`]). At most
//! [`MAX_SUBTASKS`] are kept; with more tool calls, the oldest go. Swift never parses provider
//! output; it shows this. Feed every event of one run to [`PlanView::observe`].

use crate::agent_text::AgentText;
use crate::event::{EventBody, PlanUpdated, PolicyDecision, Resolution, StepStatus};
use crate::ids::{CheckpointId, ToolCallId};
use std::collections::HashMap;

/// The most subtasks kept.
pub const MAX_SUBTASKS: usize = 100;
/// The longest subtask title, in characters.
pub const SUBTASK_TITLE_MAX_CHARS: usize = 80;

/// Where a subtask stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtaskStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    Current,
    /// Finished.
    Done,
    /// Dropped by the agent, or denied before it ran.
    Skipped,
    /// A tool call that failed.
    Failed,
}

/// One subtask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtask {
    /// The step's title or the tool's name: agent text, cleaned and capped.
    pub title: AgentText,
    /// Where it stands.
    pub status: SubtaskStatus,
}

/// Where the subtasks came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// The agent published a plan.
    Plan,
    /// The run's tool calls.
    Tools,
}

/// A run's subtasks, kept up to date from its events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanView {
    subtasks: Vec<Subtask>,
    planned: bool,
    /// Each tool call's place in `subtasks`, until a plan is published.
    calls: Vec<ToolCallId>,
    checkpoints: HashMap<CheckpointId, ToolCallId>,
}

impl PlanView {
    /// Applies one of the run's events.
    pub fn observe(&mut self, body: &EventBody) {
        match body {
            EventBody::PlanUpdated(plan) => self.plan(plan),
            EventBody::ToolRequested(requested) if !self.planned => {
                if self.subtasks.len() == MAX_SUBTASKS {
                    self.subtasks.remove(0);
                    self.calls.remove(0);
                }
                self.subtasks.push(Subtask {
                    title: AgentText::new(&requested.tool, SUBTASK_TITLE_MAX_CHARS),
                    status: SubtaskStatus::Current,
                });
                self.calls.push(requested.tool_call_id.clone());
            }
            EventBody::ToolResult(result) => {
                let status = if result.ok {
                    SubtaskStatus::Done
                } else {
                    SubtaskStatus::Failed
                };
                self.set(&result.tool_call_id, status);
            }
            EventBody::PolicyDecided(decided) => match &decided.decision {
                PolicyDecision::Deny { .. } => {
                    self.set(&decided.tool_call_id, SubtaskStatus::Skipped)
                }
                PolicyDecision::Allow | PolicyDecision::Ask { .. } => {}
            },
            EventBody::CheckpointOpened(opened) => {
                self.checkpoints
                    .insert(opened.checkpoint_id.clone(), opened.tool_call_id.clone());
            }
            EventBody::CheckpointResolved(resolved) => {
                let Some(call) = self.checkpoints.remove(&resolved.checkpoint_id) else {
                    return;
                };
                match resolved.resolution {
                    // Denied, or never answered: the tool doesn't run.
                    Resolution::Denied { .. } | Resolution::Expired => {
                        self.set(&call, SubtaskStatus::Skipped);
                    }
                    Resolution::Approved
                    | Resolution::Edited { .. }
                    | Resolution::AllowedForRun => {}
                }
            }
            EventBody::ToolRequested(_)
            | EventBody::RunQueued(_)
            | EventBody::RunStarted(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::ModelResponse(_)
            | EventBody::ToolStarted(_)
            | EventBody::UsageReported(_)
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::RunCompleted(_)
            | EventBody::RunFailed(_)
            | EventBody::RunCancelled(_)
            | EventBody::SessionClosed(_) => {}
        }
    }

    fn plan(&mut self, plan: &PlanUpdated) {
        self.planned = true;
        self.calls.clear();
        self.subtasks = plan
            .steps
            .iter()
            .take(MAX_SUBTASKS)
            .map(|step| Subtask {
                title: AgentText::new(&step.title, SUBTASK_TITLE_MAX_CHARS),
                status: match step.status {
                    StepStatus::Pending => SubtaskStatus::Pending,
                    StepStatus::InProgress => SubtaskStatus::Current,
                    StepStatus::Done => SubtaskStatus::Done,
                    StepStatus::Skipped => SubtaskStatus::Skipped,
                },
            })
            .collect();
    }

    /// Sets a tool call's step, while tool calls are the subtasks.
    fn set(&mut self, call: &ToolCallId, status: SubtaskStatus) {
        if self.planned {
            return;
        }
        if let Some(at) = self.calls.iter().position(|c| c == call) {
            self.subtasks[at].status = status;
        }
    }

    /// The subtasks, in order.
    pub fn subtasks(&self) -> &[Subtask] {
        &self.subtasks
    }

    /// How many there are.
    pub fn total(&self) -> usize {
        self.subtasks.len()
    }

    /// Where they came from; `None` while there are none.
    pub fn source(&self) -> Option<PlanSource> {
        if self.subtasks.is_empty() {
            None
        } else if self.planned {
            Some(PlanSource::Plan)
        } else {
            Some(PlanSource::Tools)
        }
    }

    /// The current subtask and its place: the plan's first in progress, or the latest tool call
    /// still running.
    pub fn current(&self) -> Option<(usize, &Subtask)> {
        let mut current = self
            .subtasks
            .iter()
            .enumerate()
            .filter(|(_, s)| s.status == SubtaskStatus::Current);
        if self.planned {
            current.next()
        } else {
            current.next_back()
        }
    }
}
