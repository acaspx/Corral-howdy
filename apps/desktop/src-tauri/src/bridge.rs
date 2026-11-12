//! The wire contract with CorralNative (Swift) over the C ABI (ADR-001, docs/01 §2).
//!
//! Pure Rust with no FFI, so it builds and is tested on every platform. The shared fixtures in
//! `native/CorralNative/Fixtures/bridge` are tested from both sides (TP-BRIDGE-01..04), so a change
//! on one side fails the other's tests. The FFI itself lives in `native.rs`.

use corral_core::controls::MAX_PROMPT_BYTES;
use corral_core::corral_view::{CorralView, DropState};
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::run_card::{CardAction, CardStatus, RunCard, RunCards};
use serde::{Deserialize, Serialize};

/// Must equal `corralNativeABIVersion` in `CorralBridge.swift`.
pub const ABI_VERSION: u32 = 2;
/// Version of the JSON payloads below.
pub const WIRE_VERSION: u32 = 1;
/// The corral never shows more than an octagon (docs/07 §3).
pub const MAX_SHOWN: usize = 8;
/// Must not exceed Swift's `CorralRoster.maxIDLength`.
pub const MAX_ID_LEN: usize = 64;
/// Largest intent payload Rust reads from Swift.
pub const MAX_INTENT_BYTES: usize = 16 * 1024;

/// A ranked roster snapshot for the corral: what Rust sends (`roster-v1.json`), with each shown
/// drop's run card when it has one (`roster-cards-v1.json`, SK-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RosterSnapshot {
    v: u32,
    agents: Vec<RosterAgent>,
    hidden: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cards: Vec<CardSnapshot>,
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
    }
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
