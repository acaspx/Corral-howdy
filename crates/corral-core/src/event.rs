//! Event schema v1: the single source of truth for everything a run does (ADR-002, system
//! design §3 and §6). Run state, hover cards, the trace tree, traces and spend are projections
//! of these events.
//!
//! On the wire an event is one JSON object:
//! `{"run_id", "seq", "ts", "span_id", "parent_span_id"?, "kind", "payload"}`.
//! Hashes belong to the store, not to this type. Match on [`EventKind`] and [`EventBody`]
//! exhaustively; never with a wildcard arm (AGENTS.md rule 8).

use crate::domain::{ActionClass, Risk};
use crate::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

pub use crate::clock::{Clock, ManualClock};

/// Why an event couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// The `kind` isn't one of the v1 kinds. Never silently accepted (TP-EVT-03).
    #[error("unknown event kind `{0}`")]
    UnknownKind(String),
    /// Not valid JSON, no `kind`, or a payload that doesn't match its kind.
    #[error("malformed event: {0}")]
    Malformed(String),
}

/// One thing that happened in a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The run this event belongs to.
    pub run_id: RunId,
    /// Position in the run's log, starting at 1 and gapless.
    pub seq: u64,
    /// When it happened, in Unix milliseconds (UTC). Comes from an injected [`Clock`].
    pub ts: u64,
    /// The trace span this event belongs to.
    pub span_id: SpanId,
    /// The enclosing span, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// What happened: the `kind` and its `payload`.
    #[serde(flatten)]
    pub body: EventBody,
}

impl Event {
    /// The event's kind.
    pub fn kind(&self) -> EventKind {
        self.body.kind()
    }

    /// Reads one event from JSON. An unknown kind is [`EventError::UnknownKind`]; anything else
    /// wrong is [`EventError::Malformed`].
    pub fn from_json(text: &str) -> Result<Self, EventError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| EventError::Malformed(e.to_string()))?;
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| EventError::Malformed("missing `kind`".into()))?;
        kind.parse::<EventKind>()?;
        serde_json::from_value(value).map_err(|e| EventError::Malformed(e.to_string()))
    }

    /// Compact JSON, one event per line in logs and IPC.
    pub fn to_json(&self) -> Result<String, EventError> {
        serde_json::to_string(self).map_err(|e| EventError::Malformed(e.to_string()))
    }

    /// Pretty JSON with a trailing newline: the golden-fixture format.
    pub fn to_json_pretty(&self) -> Result<String, EventError> {
        serde_json::to_string_pretty(self)
            .map(|s| s + "\n")
            .map_err(|e| EventError::Malformed(e.to_string()))
    }
}

/// The v1 event kinds, in the order system design §6 lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `run.queued`
    RunQueued,
    /// `run.started`
    RunStarted,
    /// `plan.updated`
    PlanUpdated,
    /// `model.request`
    ModelRequest,
    /// `model.delta`
    ModelDelta,
    /// `model.response`
    ModelResponse,
    /// `tool.requested`
    ToolRequested,
    /// `policy.decided`
    PolicyDecided,
    /// `checkpoint.opened`
    CheckpointOpened,
    /// `checkpoint.resolved`
    CheckpointResolved,
    /// `tool.started`
    ToolStarted,
    /// `tool.result`
    ToolResult,
    /// `usage.reported`
    UsageReported,
    /// `budget.warned`
    BudgetWarned,
    /// `run.paused`
    RunPaused,
    /// `run.resumed`
    RunResumed,
    /// `run.completed`
    RunCompleted,
    /// `run.failed`
    RunFailed,
    /// `run.cancelled`
    RunCancelled,
    /// `session.closed`
    SessionClosed,
}

impl EventKind {
    /// Every kind, in spec order.
    pub const ALL: [EventKind; 20] = [
        EventKind::RunQueued,
        EventKind::RunStarted,
        EventKind::PlanUpdated,
        EventKind::ModelRequest,
        EventKind::ModelDelta,
        EventKind::ModelResponse,
        EventKind::ToolRequested,
        EventKind::PolicyDecided,
        EventKind::CheckpointOpened,
        EventKind::CheckpointResolved,
        EventKind::ToolStarted,
        EventKind::ToolResult,
        EventKind::UsageReported,
        EventKind::BudgetWarned,
        EventKind::RunPaused,
        EventKind::RunResumed,
        EventKind::RunCompleted,
        EventKind::RunFailed,
        EventKind::RunCancelled,
        EventKind::SessionClosed,
    ];

    /// The kind as it appears on the wire, e.g. `"run.started"`.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::RunQueued => "run.queued",
            EventKind::RunStarted => "run.started",
            EventKind::PlanUpdated => "plan.updated",
            EventKind::ModelRequest => "model.request",
            EventKind::ModelDelta => "model.delta",
            EventKind::ModelResponse => "model.response",
            EventKind::ToolRequested => "tool.requested",
            EventKind::PolicyDecided => "policy.decided",
            EventKind::CheckpointOpened => "checkpoint.opened",
            EventKind::CheckpointResolved => "checkpoint.resolved",
            EventKind::ToolStarted => "tool.started",
            EventKind::ToolResult => "tool.result",
            EventKind::UsageReported => "usage.reported",
            EventKind::BudgetWarned => "budget.warned",
            EventKind::RunPaused => "run.paused",
            EventKind::RunResumed => "run.resumed",
            EventKind::RunCompleted => "run.completed",
            EventKind::RunFailed => "run.failed",
            EventKind::RunCancelled => "run.cancelled",
            EventKind::SessionClosed => "session.closed",
        }
    }
}

impl FromStr for EventKind {
    type Err = EventError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EventKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| EventError::UnknownKind(s.to_owned()))
    }
}

/// An event's `kind` and `payload`. The serde names must match [`EventKind::as_str`]
/// (TP-EVT-01 checks both).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
#[allow(missing_docs)] // each variant is documented on its payload type
pub enum EventBody {
    #[serde(rename = "run.queued")]
    RunQueued(RunQueued),
    #[serde(rename = "run.started")]
    RunStarted(RunStarted),
    #[serde(rename = "plan.updated")]
    PlanUpdated(PlanUpdated),
    #[serde(rename = "model.request")]
    ModelRequest(ModelRequest),
    #[serde(rename = "model.delta")]
    ModelDelta(ModelDelta),
    #[serde(rename = "model.response")]
    ModelResponse(ModelResponse),
    #[serde(rename = "tool.requested")]
    ToolRequested(ToolRequested),
    #[serde(rename = "policy.decided")]
    PolicyDecided(PolicyDecided),
    #[serde(rename = "checkpoint.opened")]
    CheckpointOpened(CheckpointOpened),
    #[serde(rename = "checkpoint.resolved")]
    CheckpointResolved(CheckpointResolved),
    #[serde(rename = "tool.started")]
    ToolStarted(ToolStarted),
    #[serde(rename = "tool.result")]
    ToolResult(ToolResult),
    #[serde(rename = "usage.reported")]
    UsageReported(UsageReported),
    #[serde(rename = "budget.warned")]
    BudgetWarned(BudgetWarned),
    #[serde(rename = "run.paused")]
    RunPaused(RunPaused),
    #[serde(rename = "run.resumed")]
    RunResumed(RunResumed),
    #[serde(rename = "run.completed")]
    RunCompleted(RunCompleted),
    #[serde(rename = "run.failed")]
    RunFailed(RunFailed),
    #[serde(rename = "run.cancelled")]
    RunCancelled(RunCancelled),
    #[serde(rename = "session.closed")]
    SessionClosed(SessionClosed),
}

impl EventBody {
    /// The body's kind.
    pub fn kind(&self) -> EventKind {
        match self {
            EventBody::RunQueued(_) => EventKind::RunQueued,
            EventBody::RunStarted(_) => EventKind::RunStarted,
            EventBody::PlanUpdated(_) => EventKind::PlanUpdated,
            EventBody::ModelRequest(_) => EventKind::ModelRequest,
            EventBody::ModelDelta(_) => EventKind::ModelDelta,
            EventBody::ModelResponse(_) => EventKind::ModelResponse,
            EventBody::ToolRequested(_) => EventKind::ToolRequested,
            EventBody::PolicyDecided(_) => EventKind::PolicyDecided,
            EventBody::CheckpointOpened(_) => EventKind::CheckpointOpened,
            EventBody::CheckpointResolved(_) => EventKind::CheckpointResolved,
            EventBody::ToolStarted(_) => EventKind::ToolStarted,
            EventBody::ToolResult(_) => EventKind::ToolResult,
            EventBody::UsageReported(_) => EventKind::UsageReported,
            EventBody::BudgetWarned(_) => EventKind::BudgetWarned,
            EventBody::RunPaused(_) => EventKind::RunPaused,
            EventBody::RunResumed(_) => EventKind::RunResumed,
            EventBody::RunCompleted(_) => EventKind::RunCompleted,
            EventBody::RunFailed(_) => EventKind::RunFailed,
            EventBody::RunCancelled(_) => EventKind::RunCancelled,
            EventBody::SessionClosed(_) => EventKind::SessionClosed,
        }
    }
}

// ---- Payloads ------------------------------------------------------------------------------
// Prompt and model text stay local; export redacts them unless the user opts in (system design §6).

/// `run.queued`: a deploy was accepted for an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunQueued {
    /// The agent being deployed.
    pub agent_id: AgentId,
    /// The session the run belongs to ("prompt again" reuses it).
    pub session_id: SessionId,
    /// The task, as the user typed or said it.
    pub prompt: String,
}

/// `run.started`: the adapter is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStarted {
    /// Which adapter runs the agent, e.g. `openai_responses`, `acp`, `fake`.
    pub adapter: String,
}

/// `plan.updated`: the agent published or revised its plan (source of the subtask cards).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanUpdated {
    /// The whole plan, in order.
    pub steps: Vec<PlanStep>,
}

/// One step of an agent's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    /// Agent-written title. Untrusted text: the UI caps and sanitizes it (security model T20).
    pub title: String,
    /// Where the step stands.
    pub status: StepStatus,
}

/// Where a plan step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Not started.
    Pending,
    /// The current step.
    InProgress,
    /// Finished.
    Done,
    /// Dropped by the agent.
    Skipped,
}

/// `model.request`: a call to the model began.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    /// Provider, e.g. `xai`.
    pub provider: String,
    /// Model name as the provider reports it.
    pub model: String,
}

/// `model.delta`: streamed output text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDelta {
    /// The new text since the last delta.
    pub text: String,
}

/// `model.response`: a model call finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    /// The full text of the response.
    pub text: String,
    /// The provider's stop reason, if it gave one.
    pub finish_reason: Option<String>,
}

/// `tool.requested`: the agent asked to use a tool. Nothing runs until policy decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequested {
    /// Ties the request, decision, checkpoint and result together.
    pub tool_call_id: ToolCallId,
    /// Tool name, e.g. `fs.write`.
    pub tool: String,
    /// The class the adapter mapped this call to.
    pub action_class: ActionClass,
    /// Arguments exactly as requested.
    pub args: serde_json::Value,
}

/// `policy.decided`: the policy engine's answer for one tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecided {
    /// The tool call decided on.
    pub tool_call_id: ToolCallId,
    /// The decision.
    pub decision: PolicyDecision,
}

/// What the policy engine decided (system design §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Runs without asking.
    Allow,
    /// Stops at a checkpoint.
    Ask {
        /// How much is at stake.
        risk: Risk,
        /// Why it asks, for the trace.
        reason: String,
    },
    /// Never runs.
    Deny {
        /// Why, for the trace and the agent.
        reason: String,
    },
}

/// `checkpoint.opened`: a decision is waiting on the user. The approval box is drawn only from
/// these fields (security model T20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointOpened {
    /// The checkpoint.
    pub checkpoint_id: CheckpointId,
    /// The tool call waiting on it.
    pub tool_call_id: ToolCallId,
    /// The action's class.
    pub action_class: ActionClass,
    /// Its risk tier, which sets the approve gesture.
    pub risk: Risk,
    /// Short title written by the core, e.g. "Send an email".
    pub title: String,
    /// The exact detail: destination, command, path, verbatim.
    pub detail: String,
}

/// `checkpoint.resolved`: the user (or a timeout) answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointResolved {
    /// The checkpoint.
    pub checkpoint_id: CheckpointId,
    /// The answer.
    pub resolution: Resolution,
}

/// How a checkpoint was answered (system design §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Resolution {
    /// Approved once.
    Approved,
    /// Denied, with an optional note sent to the agent.
    Denied {
        /// The note, if the user wrote one.
        note: Option<String>,
    },
    /// Approved with edited arguments.
    Edited {
        /// The arguments that will run instead.
        args: serde_json::Value,
    },
    /// Approved for the rest of this run (scoped per system design §5).
    AllowedForRun,
    /// Nobody answered in time.
    Expired,
}

/// `tool.started`: an allowed tool call began.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolStarted {
    /// The tool call.
    pub tool_call_id: ToolCallId,
}

/// `tool.result`: a tool call finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    /// The tool call.
    pub tool_call_id: ToolCallId,
    /// Whether it succeeded.
    pub ok: bool,
    /// The output, or a reference to a blob for large outputs (the store decides).
    pub output: serde_json::Value,
}

/// `usage.reported`: tokens and cost the provider reported for a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReported {
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    pub output_tokens: u64,
    /// Cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
}

/// `budget.warned`: spend crossed a soft limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetWarned {
    /// Which budget.
    pub scope: BudgetScope,
    /// Spent so far, in millionths of a US dollar.
    pub spent_micro_usd: u64,
    /// The limit crossed, in millionths of a US dollar.
    pub limit_micro_usd: u64,
}

/// Which budget a warning is about (system design §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// This run.
    Run,
    /// This agent, this month.
    AgentMonth,
    /// All agents.
    Global,
}

/// `run.paused`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPaused {
    /// Why it paused.
    pub reason: PauseReason,
}

/// Why a run paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// The user pressed Pause.
    User,
    /// A budget soft limit.
    Budget,
}

/// `run.resumed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunResumed {}

/// `run.completed`: the agent finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCompleted {
    /// The agent's own summary, if it gave one. Untrusted text (T20).
    pub summary: Option<String>,
}

/// `run.failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFailed {
    /// Why, as a category the UI can act on.
    pub reason: FailureReason,
    /// What happened, for the card and the trace. Never contains secrets.
    pub message: String,
}

/// Why a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// The model asked for a tool but the agent has none yet (walking skeleton, TP-SKEL-02).
    ToolsNotEnabled,
    /// The run hit its hard spend cap.
    BudgetHardCap,
    /// The provider returned an error.
    Provider,
    /// The adapter crashed or broke protocol.
    Adapter,
    /// A bug in Corral.
    Internal,
}

/// `run.cancelled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCancelled {
    /// Who or what cancelled it.
    pub by: CancelledBy,
}

/// Who or what cancelled a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelledBy {
    /// The user pressed Stop.
    User,
    /// The Recall-all hotkey or menu item.
    RecallAll,
    /// A checkpoint expired under a cancel policy.
    CheckpointExpired,
}

/// `session.closed`: the agent was sent back to the corral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionClosed {}
