//! The trace tree's columns (M4 item 3, docs/07 §8.3, TP-TRACE-02, TP-TRACE-06): which runs get a
//! column above the corral, in what order, and what each shows. The core decides; Swift draws.
//!
//! - **Which:** every agent whose run is queued, working or waiting on you, shown or hidden. A
//!   finished, failed or stopped run has no column: it folds into its drop. Nothing running, no
//!   tree.
//! - **Order:** waiting on you first, then the newest. At most [`MAX_COLUMNS`]; the rest are
//!   counted in `more` ("+N more running").
//! - **Each column:** the prompt, the current subtask with its place ("2 of 5") and up to
//!   [`MAX_PEEK`] pending ones that peek out below it, from the run's [`crate::plan_view`].

use crate::agent_text::AgentText;
use crate::ids::AgentId;
use crate::plan_view::SubtaskStatus;
use crate::run_card::{CardStatus, RunCards};
use std::cmp::Reverse;

/// The most columns side by side.
pub const MAX_COLUMNS: usize = 3;
/// The most pending subtasks peeking out below the current one.
pub const MAX_PEEK: usize = 2;

/// The current subtask in a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSubtask {
    /// Its place, from 1.
    pub step: usize,
    /// How many there are.
    pub total: usize,
    /// Its title.
    pub title: AgentText,
}

/// One column of the trace tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceColumn {
    /// The agent whose run it is; its wire ends at that drop, or the hidden bead.
    pub agent: AgentId,
    /// Queued, working or waiting on you.
    pub status: CardStatus,
    /// The task, as the card shows it.
    pub prompt: AgentText,
    /// The current subtask, once the run has one.
    pub subtask: Option<ColumnSubtask>,
    /// What's left after it, at most [`MAX_PEEK`].
    pub next: Vec<AgentText>,
}

/// The trace tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceTree {
    /// At most [`MAX_COLUMNS`], in order.
    pub columns: Vec<TraceColumn>,
    /// Running beyond those.
    pub more: usize,
}

/// The trace tree for `agents` (every agent, shown or hidden), from their cards.
pub fn trace_tree(cards: &RunCards, agents: &[AgentId]) -> TraceTree {
    let mut running: Vec<(TraceColumn, u64)> = Vec::new();
    for agent in agents {
        if running.iter().any(|(c, _)| &c.agent == agent) {
            continue;
        }
        let Some(card) = cards.card(agent, 0) else {
            continue;
        };
        match card.status {
            CardStatus::Queued | CardStatus::Working | CardStatus::NeedsYou => {}
            CardStatus::Done | CardStatus::Error | CardStatus::Stopped => continue,
        }
        let (subtask, next) = cards.plan(agent).map_or((None, Vec::new()), |plan| {
            let Some((at, current)) = plan.current() else {
                return (None, Vec::new());
            };
            // The pending ones after the current one (which isn't pending itself).
            let next = plan
                .subtasks()
                .iter()
                .skip(at)
                .filter(|s| s.status == SubtaskStatus::Pending)
                .take(MAX_PEEK)
                .map(|s| s.title.clone())
                .collect();
            let subtask = ColumnSubtask {
                step: at + 1,
                total: plan.total(),
                title: current.title.clone(),
            };
            (Some(subtask), next)
        });
        let queued = cards.queued_at(agent).unwrap_or(0);
        running.push((
            TraceColumn {
                agent: agent.clone(),
                status: card.status,
                prompt: card.prompt,
                subtask,
                next,
            },
            queued,
        ));
    }
    // Waiting on you first, then the newest; a stable sort keeps `agents`' order for ties.
    running.sort_by_key(|(c, queued)| (c.status != CardStatus::NeedsYou, Reverse(*queued)));
    let more = running.len().saturating_sub(MAX_COLUMNS);
    TraceTree {
        columns: running
            .into_iter()
            .take(MAX_COLUMNS)
            .map(|(c, _)| c)
            .collect(),
        more,
    }
}
