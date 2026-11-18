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
//! - **A waiting checkpoint** puts its approval box on the card, drawn only from
//!   `checkpoint.opened`, which the core wrote (M2 item 4, TP-CHK-07). Its title and detail are
//!   cleaned and capped like agent text, because the detail quotes the agent's arguments
//!   verbatim. With more than one waiting, the oldest shows first.
//!
//! - **Edit** (TP-CHK-13..14): while a checkpoint waits, the card says which of the call's
//!   arguments it may change, from the `tool.requested` the checkpoint is for. Each top-level text
//!   or number argument is a labeled field; text with hidden or control characters, other values
//!   and odd keys show read-only. The card sends back only what changed, and
//!   [`RunCards::edited_args`] merges it into the agent's arguments, so read-only and nested values
//!   never cross back.
//!
//! Every event is checked with the same lifecycle function the supervisor uses ([`next`]), so a
//! card can never disagree with the log. Feed every logged event to [`RunCards::observe`].

use crate::agent_text::{ANSWER_MAX_CHARS, AgentText, PROMPT_MAX_CHARS, is_plain};
use crate::budget::usd;
use crate::corral_view::ViewError;
use crate::domain::{ActionClass, Risk};
use crate::event::{Event, EventBody};
use crate::ids::{AgentId, CheckpointId, RunId, ToolCallId};
use crate::lifecycle::{Lifecycle, Phase, next};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, HashMap};

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
    /// Approve the waiting checkpoint.
    Approve,
    /// Deny the waiting checkpoint.
    Deny,
}

/// The longest checkpoint title on a card, in characters.
pub const CHECKPOINT_TITLE_MAX_CHARS: usize = 120;
/// The longest checkpoint detail on a card, in characters (the runner's cap, applied again).
pub const CHECKPOINT_DETAIL_MAX_CHARS: usize = 2_000;

/// The largest arguments a card may edit, in bytes of JSON.
pub const EDIT_ARGS_MAX_BYTES: usize = 8 * 1024;
/// The most arguments a card may edit.
pub const EDIT_MAX_FIELDS: usize = 20;
/// The longest key a card may edit, in characters. A longer one shows read-only, its label cut.
pub const FIELD_KEY_MAX_CHARS: usize = 40;
/// The most of a read-only value a card shows, in characters.
pub const FIXED_VALUE_MAX_CHARS: usize = 200;

/// What a card may do with one argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// Text the user may change.
    Text,
    /// A number the user may change.
    Number,
    /// Shown, not editable: nested values, booleans, null, and text or keys with hidden or
    /// control characters.
    Fixed,
}

/// One top-level argument of a waiting call, as the edit sheet shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardField {
    /// The argument's key, exactly. An edit names it.
    pub key: String,
    /// The key as words, e.g. "File path" for `file_path`.
    pub label: String,
    /// What the card may do with it.
    pub kind: FieldKind,
    /// The value: exact for text and numbers; cleaned and cut for read-only ones.
    pub value: String,
}

/// Why a card's edit wasn't merged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The card doesn't show that checkpoint now.
    #[error("That checkpoint isn't on this card now.")]
    NotShown,
    /// The checkpoint has no editable fields.
    #[error("This can't be edited on the card.")]
    NotEditable,
    /// The edit changes nothing.
    #[error("Nothing was changed.")]
    NoChanges,
    /// That key isn't an editable field.
    #[error("{0:?} can't be changed here.")]
    Field(String),
    /// A number field got something that isn't a number.
    #[error("{0:?} needs a number.")]
    Number(String),
    /// Edited text has hidden or control characters.
    #[error("{0:?} has hidden or control characters. Type it again.")]
    Text(String),
}

/// The approval box: a waiting checkpoint, from `checkpoint.opened` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardCheckpoint {
    /// The checkpoint, whose ID answers it once.
    pub id: CheckpointId,
    /// What kind of action it is.
    pub class: ActionClass,
    /// How much is at stake, which sets the gesture.
    pub risk: Risk,
    /// The core's title, e.g. "Sending a message or email".
    pub title: AgentText,
    /// The exact detail: tool, destination, arguments.
    pub detail: AgentText,
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
    /// The oldest checkpoint waiting on the user, while the run waits.
    pub checkpoint: Option<CardCheckpoint>,
    /// That checkpoint's arguments as fields, when the card may edit them.
    pub edit: Option<Vec<CardField>>,
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
    /// Checkpoints waiting, oldest first.
    waiting: Vec<CardCheckpoint>,
    /// The latest `tool.requested`, until its checkpoint opens.
    requested: Option<(ToolCallId, Value)>,
    /// Each waiting checkpoint's arguments and fields, when it was a tool call that can be
    /// edited.
    editable: HashMap<CheckpointId, (Value, Vec<CardField>)>,
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
                    waiting: Vec::new(),
                    requested: None,
                    editable: HashMap::new(),
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
            EventBody::ToolRequested(requested) => {
                record.requested = Some((requested.tool_call_id.clone(), requested.args.clone()));
            }
            EventBody::CheckpointOpened(opened) => {
                if let Some((call, args)) = record.requested.take()
                    && call == opened.tool_call_id
                    && let Some(fields) = fields_of(&args)
                {
                    record
                        .editable
                        .insert(opened.checkpoint_id.clone(), (args, fields));
                }
                record.waiting.push(CardCheckpoint {
                    id: opened.checkpoint_id.clone(),
                    class: opened.action_class,
                    risk: opened.risk,
                    title: AgentText::new(&opened.title, CHECKPOINT_TITLE_MAX_CHARS),
                    detail: AgentText::new(&opened.detail, CHECKPOINT_DETAIL_MAX_CHARS),
                });
            }
            EventBody::CheckpointResolved(resolved) => {
                record
                    .waiting
                    .retain(|waiting| waiting.id != resolved.checkpoint_id);
                record.editable.remove(&resolved.checkpoint_id);
            }
            EventBody::RunQueued(_)
            | EventBody::PlanUpdated(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::PolicyDecided(_)
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
        // A run that has ended waits on nothing, whatever was open when it stopped.
        let checkpoint = (status == CardStatus::NeedsYou)
            .then(|| record.waiting.first().cloned())
            .flatten();
        let edit = checkpoint
            .as_ref()
            .and_then(|shown| record.editable.get(&shown.id))
            .map(|(_, fields)| fields.clone());
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
            checkpoint,
            edit,
        })
    }

    /// The agent's arguments for the checkpoint `agent`'s card shows, with the card's `changes`
    /// (key to the text typed) merged in (TP-CHK-14). Only editable fields can change; a number
    /// field must get a number; edited text must be plain. Everything else stays the agent's.
    pub fn edited_args(
        &self,
        agent: &AgentId,
        checkpoint: &CheckpointId,
        changes: &BTreeMap<String, String>,
    ) -> Result<Value, EditError> {
        if !self.shows_checkpoint(agent, checkpoint) {
            return Err(EditError::NotShown);
        }
        let (args, fields) = self
            .latest
            .get(agent)
            .and_then(|run| self.runs.get(run))
            .and_then(|record| record.editable.get(checkpoint))
            .ok_or(EditError::NotEditable)?;
        if changes.is_empty() {
            return Err(EditError::NoChanges);
        }
        let mut args = args.clone();
        for (key, typed) in changes {
            let kind = fields
                .iter()
                .find(|field| field.key == *key)
                .map(|field| field.kind);
            let value = match kind {
                Some(FieldKind::Text) if is_plain(typed) => Value::String(typed.clone()),
                Some(FieldKind::Text) => return Err(EditError::Text(key.clone())),
                Some(FieldKind::Number) => serde_json::from_str::<Number>(typed.trim())
                    .map(Value::Number)
                    // The typed text isn't a number; the card shows why.
                    .map_err(|_| EditError::Number(key.clone()))?,
                Some(FieldKind::Fixed) | None => return Err(EditError::Field(key.clone())),
            };
            if let Value::Object(map) = &mut args {
                map.insert(key.clone(), value);
            }
        }
        Ok(args)
    }

    /// Whether `checkpoint` is the one `agent`'s card shows now, so an answer from that card is
    /// for what the user saw (TP-HOVER-06).
    pub fn shows_checkpoint(&self, agent: &AgentId, checkpoint: &CheckpointId) -> bool {
        self.card(agent, 0)
            .and_then(|card| card.checkpoint)
            .is_some_and(|shown| shown.id == *checkpoint)
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

/// A call's arguments as fields, when a card may edit them: an object of at most
/// [`EDIT_MAX_FIELDS`] keys and [`EDIT_ARGS_MAX_BYTES`] bytes, with at least one editable value.
fn fields_of(args: &Value) -> Option<Vec<CardField>> {
    let Value::Object(map) = args else {
        return None;
    };
    if map.len() > EDIT_MAX_FIELDS || args.to_string().len() > EDIT_ARGS_MAX_BYTES {
        return None;
    }
    let fields = fields(map);
    fields
        .iter()
        .any(|field| field.kind != FieldKind::Fixed)
        .then_some(fields)
}

fn fields(map: &Map<String, Value>) -> Vec<CardField> {
    map.iter()
        .map(|(key, value)| {
            let key_ok =
                is_plain(key) && !key.contains('\n') && key.chars().count() <= FIELD_KEY_MAX_CHARS;
            let (kind, value) = match value {
                Value::String(text) if key_ok && is_plain(text) => (FieldKind::Text, text.clone()),
                Value::Number(number) if key_ok => (FieldKind::Number, number.to_string()),
                Value::String(text) => (FieldKind::Fixed, fixed(text)),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => (FieldKind::Fixed, fixed(&value.to_string())),
            };
            CardField {
                key: key.clone(),
                label: label(key),
                kind,
                value,
            }
        })
        .collect()
}

fn fixed(text: &str) -> String {
    AgentText::new(text, FIXED_VALUE_MAX_CHARS)
        .as_str()
        .to_owned()
}

/// A key as words: `file_path`, `max-bytes` and `dryRun` read "File path", "Max bytes" and
/// "Dry run". Cleaned and cut first, because a key is agent text.
fn label(key: &str) -> String {
    let clean = AgentText::new(key, FIELD_KEY_MAX_CHARS);
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut after_lower = false;
    for c in clean.as_str().chars() {
        if c == '_' || c == '-' || c.is_whitespace() {
            words.push(std::mem::take(&mut word));
            after_lower = false;
            continue;
        }
        if c.is_uppercase() && after_lower {
            words.push(std::mem::take(&mut word));
        }
        after_lower = c.is_lowercase();
        word.extend(c.to_lowercase());
    }
    words.push(word);
    let label = words
        .into_iter()
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
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
        CardStatus::NeedsYou => vec![CardAction::Approve, CardAction::Deny],
        CardStatus::Done | CardStatus::Stopped => vec![CardAction::SendBack, CardAction::NewTask],
        CardStatus::Error => vec![CardAction::Retry, CardAction::SendBack],
    }
}
