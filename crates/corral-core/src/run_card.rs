//! Run cards: what a drop's hover card and result card show, projected from the event log
//! (SK-6, TP-SKEL-04..05, docs/07 §8.2).
//!
//! Each agent's card follows its most recently deployed run until the session is sent back:
//! - **Status and buttons** from the run's phase (07 §8.2): working shows Stop; done shows Send
//!   back and New task; an error shows Retry and Send back; a stopped run shows Send back and New
//!   task.
//! - **Elapsed time** counts from `run.started` to now, and stops at the run's end. **Spend** is
//!   the sum of `usage.reported`. Both come from here, never from a surface's own counting.
//! - **The answer** is the last `model.response` text (or the completion summary if there was
//!   none), and errors are the `run.failed` message. Both are agent text: cleaned and capped
//!   ([`AgentText`]).
//!
//! Every event is checked with the same lifecycle function the supervisor uses ([`next`]), so a
//! card can never disagree with the log. Feed every logged event to [`RunCards::observe`].

use crate::agent_text::{ANSWER_MAX_CHARS, AgentText, PROMPT_MAX_CHARS};
use crate::budget::usd;
use crate::corral_view::ViewError;
use crate::event::{Event, EventBody};
use crate::ids::{AgentId, RunId};
use crate::lifecycle::{Lifecycle, Phase, next};
use std::collections::HashMap;

/// Where a card's run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardStatus {
    /// Waiting for a free slot.
    Queued,
    /// Working (or paused).
    Working,
    /// A checkpoint is waiting on the user.
    NeedsYou,
    /// Finished with an answer.
    Done,
    /// Ended with an error.
    Error,
    /// Stopped by the user or Recall-all.
    Stopped,
}

/// A button on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardAction {
    /// Stop the run.
    Stop,
    /// Close the session; the drop is ready for a task again.
    SendBack,
    /// Ask for another task (a follow-up in the same session, or a fresh one after Stop).
    NewTask,
    /// Run the failed prompt again, in the same session.
    Retry,
}

/// What one drop's card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCard {
    /// The agent.
    pub agent: AgentId,
    /// The run the card is about.
    pub run: RunId,
    /// Where it stands.
    pub status: CardStatus,
    /// The task as the user gave it, cleaned and capped for display.
    pub prompt: AgentText,
    /// From `run.started` until now, or until the run ended. Zero while queued.
    pub elapsed_ms: u64,
    /// Billed so far, in micro-USD.
    pub spend_micro_usd: u64,
    /// Billed so far, in dollars for display.
    pub spend: String,
    /// The answer, once the run has one.
    pub answer: Option<AgentText>,
    /// Why the run failed.
    pub error: Option<AgentText>,
    /// The buttons, in display order.
    pub actions: Vec<CardAction>,
}

#[derive(Debug)]
struct Record {
    agent: AgentId,
    life: Lifecycle,
    prompt: String,
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    spend_micro_usd: u64,
    answer: Option<String>,
    error: Option<String>,
}

/// Keeps every agent's card up to date from the event stream.
#[derive(Debug, Default)]
pub struct RunCards {
    runs: HashMap<RunId, Record>,
    latest: HashMap<AgentId, RunId>,
}

impl RunCards {
    /// Applies one logged event. Returns `true` when what a card shows changed, apart from the
    /// clock: streamed text and `model.request` don't count, so callers republish only when
    /// needed (and tick once a second while [`Self::is_ticking`]). A refused event changes
    /// nothing.
    pub fn observe(&mut self, event: &Event) -> Result<bool, ViewError> {
        let agent = match &event.body {
            EventBody::RunQueued(queued) => queued.agent_id.clone(),
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
            | EventBody::SessionClosed(_) => self
                .runs
                .get(&event.run_id)
                .map(|r| r.agent.clone())
                .ok_or_else(|| ViewError::UnknownRun(event.run_id.clone()))?,
        };
        let before = self.card(&agent, 0);
        self.apply(event)?;
        Ok(self.card(&agent, 0) != before)
    }

    fn apply(&mut self, event: &Event) -> Result<(), ViewError> {
        if let EventBody::RunQueued(queued) = &event.body {
            let life = next(&Lifecycle::idle(), event)?;
            self.runs.insert(
                event.run_id.clone(),
                Record {
                    agent: queued.agent_id.clone(),
                    life,
                    prompt: queued.prompt.clone(),
                    started_ms: None,
                    ended_ms: None,
                    spend_micro_usd: 0,
                    answer: None,
                    error: None,
                },
            );
            self.latest
                .insert(queued.agent_id.clone(), event.run_id.clone());
            return Ok(());
        }
        let record = self
            .runs
            .get_mut(&event.run_id)
            .ok_or_else(|| ViewError::UnknownRun(event.run_id.clone()))?;
        record.life = next(&record.life, event)?;
        match &event.body {
            EventBody::RunStarted(_) => record.started_ms = Some(event.ts),
            EventBody::ModelResponse(response) => record.answer = Some(response.text.clone()),
            EventBody::UsageReported(usage) => {
                record.spend_micro_usd =
                    record.spend_micro_usd.saturating_add(usage.cost_micro_usd);
            }
            EventBody::RunCompleted(done) => {
                record.ended_ms = Some(event.ts);
                if record.answer.is_none() {
                    record.answer.clone_from(&done.summary);
                }
            }
            EventBody::RunFailed(failed) => {
                record.ended_ms = Some(event.ts);
                record.error = Some(failed.message.clone());
            }
            EventBody::RunCancelled(_) => record.ended_ms = Some(event.ts),
            EventBody::RunQueued(_)
            | EventBody::PlanUpdated(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::ToolRequested(_)
            | EventBody::PolicyDecided(_)
            | EventBody::CheckpointOpened(_)
            | EventBody::CheckpointResolved(_)
            | EventBody::ToolStarted(_)
            | EventBody::ToolResult(_)
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::SessionClosed(_) => {}
        }
        Ok(())
    }

    /// `agent`'s card at `now_ms`, or `None` when it's in the corral with no session.
    pub fn card(&self, agent: &AgentId, now_ms: u64) -> Option<RunCard> {
        let run = self.latest.get(agent)?;
        let record = self.runs.get(run)?;
        let status = status(record.life.phase())?;
        let elapsed_ms = record.started_ms.map_or(0, |start| {
            record.ended_ms.unwrap_or(now_ms).saturating_sub(start)
        });
        Some(RunCard {
            agent: agent.clone(),
            run: run.clone(),
            status,
            prompt: AgentText::new(&record.prompt, PROMPT_MAX_CHARS),
            elapsed_ms,
            spend_micro_usd: record.spend_micro_usd,
            spend: usd(record.spend_micro_usd),
            answer: record
                .answer
                .as_deref()
                .map(|a| AgentText::new(a, ANSWER_MAX_CHARS)),
            error: record
                .error
                .as_deref()
                .map(|e| AgentText::new(e, PROMPT_MAX_CHARS)),
            actions: actions(status),
        })
    }

    /// Whether some card's clock is moving, so it needs republishing once a second.
    pub fn is_ticking(&self) -> bool {
        self.latest.values().any(|run| {
            self.runs
                .get(run)
                .is_some_and(|r| r.started_ms.is_some() && r.ended_ms.is_none())
        })
    }
}

fn status(phase: Phase) -> Option<CardStatus> {
    match phase {
        Phase::Idle => None,
        Phase::Queued => Some(CardStatus::Queued),
        Phase::Running | Phase::Paused => Some(CardStatus::Working),
        Phase::AwaitingCheckpoint => Some(CardStatus::NeedsYou),
        Phase::Completed => Some(CardStatus::Done),
        Phase::Failed => Some(CardStatus::Error),
        Phase::Cancelled => Some(CardStatus::Stopped),
    }
}

/// The buttons for each state (07 §8.2). Approve, Deny and Pause arrive with M2.
fn actions(status: CardStatus) -> Vec<CardAction> {
    match status {
        CardStatus::Queued | CardStatus::Working => vec![CardAction::Stop],
        CardStatus::NeedsYou => Vec::new(),
        CardStatus::Done | CardStatus::Stopped => vec![CardAction::SendBack, CardAction::NewTask],
        CardStatus::Error => vec![CardAction::Retry, CardAction::SendBack],
    }
}
