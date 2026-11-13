//! The corral view: what each drop shows, projected from the event log (SK-2b, docs/07 §3).
//!
//! The core decides what the corral shows (AGENTS.md rule 3); Swift only draws it. Feed every
//! event the supervisor logs to [`CorralTracker::observe`], then read [`CorralTracker::view`].
//!
//! - **A drop's state** comes from its agent's runs: any run waiting on a checkpoint shows
//!   [`DropState::NeedsYou`]; otherwise any run queued, running or paused shows
//!   [`DropState::Working`]; otherwise the most recently deployed run's outcome shows (Done, Error,
//!   or Available once it was cancelled or sent back).
//! - **Each run follows the same lifecycle checks the supervisor applies** ([`next`]), so the view
//!   can never disagree with the log. An event the lifecycle refuses changes nothing.
//! - **Shown set:** the first [`MAX_SHOWN`] agents in the order they were added (docs/07 §3.2);
//!   the rest count as hidden. Usage ranking (TP-USAGE-*) will reorder this list later without
//!   changing the API.

use crate::event::{Event, EventBody};
use crate::ids::{AgentId, RunId};
use crate::lifecycle::{Lifecycle, LifecycleError, Phase, next};
use std::collections::HashMap;

/// The corral never shows more than an octagon (docs/07 §3).
pub const MAX_SHOWN: usize = 8;

/// An agent as the corral shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCard {
    /// The agent.
    pub id: AgentId,
    /// Display name, for the hover card and VoiceOver.
    pub name: String,
    /// One to three characters drawn on the drop.
    pub monogram: String,
}

/// What a drop shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropState {
    /// In the corral, ready to deploy.
    Available,
    /// Queued, running or paused: the working ring.
    Working,
    /// A checkpoint is waiting on the user: the pulse and badge.
    NeedsYou,
    /// The latest run finished; its result can be read, or the agent prompted again.
    Done,
    /// The latest run failed.
    Error,
}

/// One drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropView {
    /// The agent.
    pub agent: AgentCard,
    /// What it shows.
    pub state: DropState,
}

/// Everything the corral shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CorralView {
    /// Shown drops, in slot order (rank 1 first), at most [`MAX_SHOWN`].
    pub drops: Vec<DropView>,
    /// Agents that exist but aren't shown.
    pub hidden: usize,
}

/// Why an event couldn't be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewError {
    /// An event for a run that was never queued.
    #[error("no run {0} was queued")]
    UnknownRun(RunId),
    /// The run's lifecycle refused the event.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
}

#[derive(Debug)]
struct RunState {
    agent: AgentId,
    life: Lifecycle,
    /// Deploy order, to find each agent's most recent run.
    order: u64,
}

/// Keeps the corral view up to date from the event stream.
#[derive(Debug)]
pub struct CorralTracker {
    agents: Vec<AgentCard>,
    runs: HashMap<RunId, RunState>,
    next_order: u64,
    shown: CorralView,
}

impl CorralTracker {
    /// A tracker for these agents, in the order they were added. A repeated ID keeps its first card.
    pub fn new(agents: Vec<AgentCard>) -> Self {
        let mut unique: Vec<AgentCard> = Vec::with_capacity(agents.len());
        for card in agents {
            if !unique.iter().any(|c| c.id == card.id) {
                unique.push(card);
            }
        }
        let mut tracker = Self {
            agents: unique,
            runs: HashMap::new(),
            next_order: 0,
            shown: CorralView::default(),
        };
        tracker.shown = tracker.compute();
        tracker
    }

    /// Replaces the corral's agents (keys connected or removed). Runs keep their state, so an
    /// agent that comes back shows what its run is doing. Returns `true` when the view changed.
    pub fn set_agents(&mut self, agents: Vec<AgentCard>) -> bool {
        let mut unique: Vec<AgentCard> = Vec::with_capacity(agents.len());
        for card in agents {
            if !unique.iter().any(|c| c.id == card.id) {
                unique.push(card);
            }
        }
        self.agents = unique;
        let view = self.compute();
        let changed = view != self.shown;
        self.shown = view;
        changed
    }

    /// Applies one logged event. Returns `true` when the view changed, so callers republish only
    /// then (streamed model text, for example, never changes a drop). A refused event changes
    /// nothing.
    pub fn observe(&mut self, event: &Event) -> Result<bool, ViewError> {
        let phase_changed = match &event.body {
            EventBody::RunQueued(queued) => {
                let life = next(&Lifecycle::idle(), event)?;
                self.runs.insert(
                    event.run_id.clone(),
                    RunState {
                        agent: queued.agent_id.clone(),
                        life,
                        order: self.next_order,
                    },
                );
                self.next_order += 1;
                true
            }
            EventBody::RunStarted(_)
            | EventBody::PlanUpdated(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::ModelResponse(_)
            | EventBody::ToolRequested(_)
            | EventBody::PolicyDecided(_)
            | EventBody::CheckpointOpened(_)
            | EventBody::CheckpointResolved(_)
            | EventBody::ToolStarted(_)
            | EventBody::ToolResult(_)
            | EventBody::UsageReported(_)
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::RunCompleted(_)
            | EventBody::RunFailed(_)
            | EventBody::RunCancelled(_)
            | EventBody::SessionClosed(_) => {
                let run = self
                    .runs
                    .get_mut(&event.run_id)
                    .ok_or_else(|| ViewError::UnknownRun(event.run_id.clone()))?;
                let life = next(&run.life, event)?;
                let changed = life.phase() != run.life.phase();
                run.life = life;
                changed
            }
        };
        if !phase_changed {
            return Ok(false);
        }
        let view = self.compute();
        let changed = view != self.shown;
        self.shown = view;
        Ok(changed)
    }

    /// The current view.
    pub fn view(&self) -> CorralView {
        self.shown.clone()
    }

    fn compute(&self) -> CorralView {
        let drops = self
            .agents
            .iter()
            .take(MAX_SHOWN)
            .map(|agent| DropView {
                agent: agent.clone(),
                state: self.state_of(&agent.id),
            })
            .collect();
        CorralView {
            drops,
            hidden: self.agents.len().saturating_sub(MAX_SHOWN),
        }
    }

    fn state_of(&self, agent: &AgentId) -> DropState {
        let mut runs: Vec<&RunState> = self.runs.values().filter(|r| &r.agent == agent).collect();
        if runs
            .iter()
            .any(|r| r.life.phase() == Phase::AwaitingCheckpoint)
        {
            return DropState::NeedsYou;
        }
        if runs.iter().any(|r| is_working(r.life.phase())) {
            return DropState::Working;
        }
        runs.sort_by_key(|r| r.order);
        runs.last()
            .map_or(DropState::Available, |latest| outcome(latest.life.phase()))
    }
}

fn is_working(phase: Phase) -> bool {
    match phase {
        Phase::Queued | Phase::Running | Phase::Paused => true,
        Phase::Idle
        | Phase::AwaitingCheckpoint
        | Phase::Completed
        | Phase::Failed
        | Phase::Cancelled => false,
    }
}

/// A finished (or never started) run's drop state.
fn outcome(phase: Phase) -> DropState {
    match phase {
        Phase::Completed => DropState::Done,
        Phase::Failed => DropState::Error,
        Phase::Idle | Phase::Cancelled => DropState::Available,
        // Not reached: active runs are handled before the latest run is consulted.
        Phase::Queued | Phase::Running | Phase::Paused => DropState::Working,
        Phase::AwaitingCheckpoint => DropState::NeedsYou,
    }
}
