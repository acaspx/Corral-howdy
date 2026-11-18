//! Touch ID for critical checkpoints (M2 item 4, TP-CHK-17; ADR-001: "Rust asks, Swift prompts,
//! Rust decides on the result").
//!
//! "Approve with Touch ID" on a card sends `approveWithTouchId`. [`TouchIdDesk::begin`] checks that
//! the card shows that checkpoint now and that it's critical, and mints a single-use request for
//! Swift's macOS prompt. The prompt asks for Touch ID; on a Mac where Touch ID can't be used (no
//! sensor, a closed laptop, none enrolled, locked out) macOS asks for the login password instead
//! (decided 2026-11-18). The password goes into macOS's own prompt, never Corral. When the result
//! comes back, [`TouchIdDesk::finish`] matches it to the request it was for, once. Only `ok`
//! approves, and then the desk and the policy check it like any answer, with
//! `Approval::TouchId`. One prompt is up at a time.

use crate::bridge::{AuthOutcome, AuthRequest};
use corral_core::agent_text::AgentText;
use corral_core::domain::{ActionClass, Risk};
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::run_card::RunCard;

/// How much of the agent's name the prompt shows.
const NAME_MAX_CHARS: usize = 40;

/// Why no prompt was started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TouchIdError {
    /// The card doesn't show that checkpoint now.
    #[error("that card doesn't show that checkpoint now")]
    NotShown,
    /// Only critical actions ask for Touch ID.
    #[error("only a critical action asks for Touch ID; approve this one with a click")]
    NotCritical,
    /// A prompt is already up.
    #[error("a Touch ID prompt is already up")]
    Busy,
}

/// What a prompt's result means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    /// Authenticated: answer this checkpoint with `Approval::TouchId`, once.
    Approve {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint it showed.
        checkpoint: CheckpointId,
    },
    /// Cancelled, not matched, or no way to authenticate on this Mac. The checkpoint waits.
    NotApproved {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint it showed.
        checkpoint: CheckpointId,
        /// What macOS said.
        outcome: AuthOutcome,
    },
    /// No prompt with that request is up: a stale or repeated result. Ignored.
    Stale,
}

#[derive(Debug)]
struct Up {
    request: String,
    agent: AgentId,
    checkpoint: CheckpointId,
}

/// The Touch ID prompt that's up, if any. One per app.
#[derive(Debug, Default)]
pub struct TouchIdDesk {
    asked: u64,
    up: Option<Up>,
}

impl TouchIdDesk {
    /// Starts a prompt for `checkpoint` on `card`, whose agent is called `name`: only the critical
    /// checkpoint the card shows now, and only when no prompt is up. The reason completes macOS's
    /// sentence "Corral is trying to …" in the core's words.
    pub fn begin(
        &mut self,
        card: &RunCard,
        checkpoint: &CheckpointId,
        name: &str,
    ) -> Result<AuthRequest, TouchIdError> {
        let shown = card
            .checkpoint
            .as_ref()
            .filter(|shown| shown.id == *checkpoint)
            .ok_or(TouchIdError::NotShown)?;
        if shown.risk != Risk::Critical {
            return Err(TouchIdError::NotCritical);
        }
        if self.up.is_some() {
            return Err(TouchIdError::Busy);
        }
        self.asked += 1;
        let request = format!("auth-{}", self.asked);
        self.up = Some(Up {
            request: request.clone(),
            agent: card.agent.clone(),
            checkpoint: shown.id.clone(),
        });
        Ok(AuthRequest::new(request, reason(shown.class, name)))
    }

    /// Drops the prompt that's up: Swift couldn't show it. A late result for it is stale.
    pub fn abandon(&mut self) {
        self.up = None;
    }

    /// Matches a prompt's result to the request it answers, once.
    pub fn finish(&mut self, request: &str, outcome: AuthOutcome) -> Finished {
        let Some(up) = self.up.take_if(|up| up.request == request) else {
            return Finished::Stale;
        };
        match outcome {
            AuthOutcome::Ok => Finished::Approve {
                agent: up.agent,
                checkpoint: up.checkpoint,
            },
            AuthOutcome::Cancelled | AuthOutcome::Failed | AuthOutcome::Unavailable => {
                Finished::NotApproved {
                    agent: up.agent,
                    checkpoint: up.checkpoint,
                    outcome,
                }
            }
        }
    }
}

/// "approve spending money for Scout": the core's title for the class, then the agent's name,
/// cleaned and cut.
fn reason(class: ActionClass, name: &str) -> String {
    let title = corral_policy::title(class);
    let mut chars = title.chars();
    let what: String = chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default();
    let name = AgentText::new(name, NAME_MAX_CHARS);
    format!("approve {what} for {}", name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use corral_core::domain::ActionClass;
    use corral_core::event::*;
    use corral_core::ids::{RunId, SessionId, SpanId, ToolCallId};
    use corral_core::run_card::{RunCard, RunCards};

    fn ev(body: EventBody) -> Event {
        Event {
            run_id: RunId::from("r1"),
            seq: 1,
            ts: 1_000,
            span_id: SpanId::from("span"),
            parent_span_id: None,
            body,
        }
    }

    /// Scout's card, waiting on checkpoint `cp-1` of `class` at `risk`.
    fn card(class: ActionClass, risk: Risk) -> RunCard {
        let mut cards = RunCards::default();
        for body in [
            EventBody::RunQueued(RunQueued {
                agent_id: AgentId::from("scout"),
                session_id: SessionId::from("s1"),
                prompt: "Buy the tickets.".into(),
            }),
            EventBody::RunStarted(RunStarted {
                adapter: "fake".into(),
            }),
            EventBody::CheckpointOpened(CheckpointOpened {
                checkpoint_id: CheckpointId::from("cp-1"),
                tool_call_id: ToolCallId::from("call-1"),
                action_class: class,
                risk,
                title: "Spending money".into(),
                detail: "payment.send to store.example".into(),
            }),
        ] {
            cards.observe(&ev(body)).expect("legal");
        }
        cards.card(&AgentId::from("scout"), 2_000).expect("card")
    }

    fn spend() -> RunCard {
        card(ActionClass::MoneySpend, Risk::Critical)
    }

    fn cp() -> CheckpointId {
        CheckpointId::from("cp-1")
    }

    #[test]
    fn tp_chk_17_a_critical_checkpoint_gets_a_prompt_in_the_cores_words() {
        let mut desk = TouchIdDesk::default();
        assert_eq!(
            desk.begin(&spend(), &cp(), "Scout"),
            Ok(AuthRequest::new(
                "auth-1",
                "approve spending money for Scout"
            ))
        );
        // A delete outside the sandbox reads the same way.
        let mut desk = TouchIdDesk::default();
        assert_eq!(
            desk.begin(
                &card(ActionClass::DeleteLocal, Risk::Critical),
                &cp(),
                "Scout"
            ),
            Ok(AuthRequest::new(
                "auth-1",
                "approve deleting a file for Scout"
            ))
        );
    }

    #[test]
    fn tp_chk_17_the_agents_name_is_cleaned_and_cut_in_the_prompt() {
        let mut desk = TouchIdDesk::default();
        let name = format!("Sc\u{202E}out{}", "x".repeat(60));
        let request = desk.begin(&spend(), &cp(), &name).expect("asked");
        let want = format!("approve spending money for Scout{}", "x".repeat(35));
        assert_eq!(request, AuthRequest::new("auth-1", want));
    }

    #[test]
    fn tp_chk_17_only_the_critical_checkpoint_the_card_shows_asks() {
        let mut desk = TouchIdDesk::default();
        assert_eq!(
            desk.begin(&spend(), &CheckpointId::from("cp-2"), "Scout"),
            Err(TouchIdError::NotShown)
        );
        assert_eq!(
            desk.begin(&card(ActionClass::CommsSend, Risk::High), &cp(), "Scout"),
            Err(TouchIdError::NotCritical),
            "a click approves high risk; Touch ID isn't a way around pinning"
        );
        assert_eq!(
            desk.begin(&card(ActionClass::ReadLocal, Risk::Normal), &cp(), "Scout"),
            Err(TouchIdError::NotCritical)
        );
        // Nothing was minted for those.
        assert_eq!(
            desk.begin(&spend(), &cp(), "Scout"),
            Ok(AuthRequest::new(
                "auth-1",
                "approve spending money for Scout"
            ))
        );
    }

    #[test]
    fn tp_chk_17_ok_approves_once_for_the_request_it_answers() {
        let mut desk = TouchIdDesk::default();
        desk.begin(&spend(), &cp(), "Scout").expect("asked");
        assert_eq!(desk.finish("auth-2", AuthOutcome::Ok), Finished::Stale);
        assert_eq!(
            desk.finish("auth-1", AuthOutcome::Ok),
            Finished::Approve {
                agent: AgentId::from("scout"),
                checkpoint: cp(),
            }
        );
        assert_eq!(
            desk.finish("auth-1", AuthOutcome::Ok),
            Finished::Stale,
            "a repeated result approves nothing"
        );
    }

    #[test]
    fn tp_chk_17_anything_but_ok_leaves_the_checkpoint_waiting() {
        for outcome in [
            AuthOutcome::Cancelled,
            AuthOutcome::Failed,
            AuthOutcome::Unavailable,
        ] {
            let mut desk = TouchIdDesk::default();
            desk.begin(&spend(), &cp(), "Scout").expect("asked");
            assert_eq!(
                desk.finish("auth-1", outcome),
                Finished::NotApproved {
                    agent: AgentId::from("scout"),
                    checkpoint: cp(),
                    outcome,
                },
                "{outcome:?}"
            );
            // The prompt is down, so the user can try again with a new request.
            assert_eq!(
                desk.begin(&spend(), &cp(), "Scout"),
                Ok(AuthRequest::new(
                    "auth-2",
                    "approve spending money for Scout"
                ))
            );
        }
    }

    #[test]
    fn tp_chk_17_one_prompt_at_a_time_until_it_answers_or_is_abandoned() {
        let mut desk = TouchIdDesk::default();
        desk.begin(&spend(), &cp(), "Scout").expect("asked");
        assert_eq!(
            desk.begin(&spend(), &cp(), "Scout"),
            Err(TouchIdError::Busy)
        );
        // Swift couldn't show it: the request is dropped, and its late result approves nothing.
        desk.abandon();
        assert_eq!(desk.finish("auth-1", AuthOutcome::Ok), Finished::Stale);
        assert_eq!(
            desk.begin(&spend(), &cp(), "Scout"),
            Ok(AuthRequest::new(
                "auth-2",
                "approve spending money for Scout"
            ))
        );
    }

    #[test]
    fn tp_chk_17_the_errors_say_what_happened() {
        assert_eq!(
            TouchIdError::NotShown.to_string(),
            "that card doesn't show that checkpoint now"
        );
        assert_eq!(
            TouchIdError::NotCritical.to_string(),
            "only a critical action asks for Touch ID; approve this one with a click"
        );
        assert_eq!(
            TouchIdError::Busy.to_string(),
            "a Touch ID prompt is already up"
        );
    }
}
