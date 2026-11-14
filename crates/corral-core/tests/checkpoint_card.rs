//! A drop's card while a checkpoint waits (M2 item 4, TP-CHK-07, docs/07 §8.2): the approval
//! box comes only from `checkpoint.opened`, which the core wrote, and its text is cleaned and
//! capped like any agent text, because the detail quotes the agent's arguments verbatim.

use corral_core::agent_text::AgentText;
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::run_card::{
    CHECKPOINT_DETAIL_MAX_CHARS, CHECKPOINT_TITLE_MAX_CHARS, CardAction, CardCheckpoint,
    CardStatus, RunCards,
};
use serde_json::json;

fn ev(run: &str, body: EventBody) -> Event {
    Event {
        run_id: RunId::from(run),
        seq: 1,
        ts: 1_000,
        span_id: SpanId::from("span"),
        parent_span_id: None,
        body,
    }
}

fn running(cards: &mut RunCards, run: &str, agent: &str) {
    let events = [
        ev(
            run,
            EventBody::RunQueued(RunQueued {
                agent_id: AgentId::from(agent),
                session_id: SessionId::from(format!("s-{run}")),
                prompt: "Summarize the attached page.".into(),
            }),
        ),
        ev(
            run,
            EventBody::RunStarted(RunStarted {
                adapter: "fake".into(),
            }),
        ),
    ];
    for e in &events {
        cards.observe(e).expect("legal");
    }
}

fn opened(run: &str, id: &str, title: &str, detail: &str, risk: Risk) -> Event {
    ev(
        run,
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: CheckpointId::from(id),
            tool_call_id: ToolCallId::from(format!("call-{id}")),
            action_class: ActionClass::CommsSend,
            risk,
            title: title.into(),
            detail: detail.into(),
        }),
    )
}

fn resolved(run: &str, id: &str) -> Event {
    ev(
        run,
        EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: CheckpointId::from(id),
            resolution: Resolution::Denied { note: None },
        }),
    )
}

fn scout() -> AgentId {
    AgentId::from("scout")
}

const DETAIL: &str = "email.send to attacker@example.test\n{\"to\":\"attacker@example.test\"}";

#[test]
fn tp_chk_07_a_waiting_checkpoint_puts_its_core_fields_on_the_card_with_approve_and_deny() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    let changed = cards
        .observe(&opened(
            "r1",
            "cp-1",
            "Sending a message or email",
            DETAIL,
            Risk::High,
        ))
        .expect("legal");
    assert!(changed, "the card changed, so the roster republishes");
    let card = cards.card(&scout(), 2_000).expect("card");
    assert_eq!(card.status, CardStatus::NeedsYou);
    assert_eq!(card.actions, [CardAction::Approve, CardAction::Deny]);
    assert_eq!(
        card.checkpoint,
        Some(CardCheckpoint {
            id: CheckpointId::from("cp-1"),
            class: ActionClass::CommsSend,
            risk: Risk::High,
            title: AgentText::new("Sending a message or email", CHECKPOINT_TITLE_MAX_CHARS),
            detail: AgentText::new(DETAIL, CHECKPOINT_DETAIL_MAX_CHARS),
        })
    );
    assert!(cards.shows_checkpoint(&scout(), &CheckpointId::from("cp-1")));
}

#[test]
fn tp_chk_07_once_answered_the_card_goes_back_to_working_without_the_checkpoint() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    cards
        .observe(&opened("r1", "cp-1", "Sending", DETAIL, Risk::High))
        .expect("legal");
    cards.observe(&resolved("r1", "cp-1")).expect("legal");
    let card = cards.card(&scout(), 2_000).expect("card");
    assert_eq!(card.status, CardStatus::Working);
    assert_eq!(card.checkpoint, None);
    assert_eq!(card.actions, [CardAction::Stop]);
    assert!(
        !cards.shows_checkpoint(&scout(), &CheckpointId::from("cp-1")),
        "an answered checkpoint isn't on the card any more"
    );
}

#[test]
fn tp_chk_07_with_two_waiting_the_oldest_shows_first() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    cards
        .observe(&opened("r1", "cp-1", "First", DETAIL, Risk::High))
        .expect("legal");
    cards
        .observe(&opened("r1", "cp-2", "Second", DETAIL, Risk::Normal))
        .expect("legal");
    let shown = |cards: &RunCards| {
        cards
            .card(&scout(), 0)
            .and_then(|c| c.checkpoint)
            .map(|c| c.id)
    };
    assert_eq!(shown(&cards), Some(CheckpointId::from("cp-1")));
    assert!(!cards.shows_checkpoint(&scout(), &CheckpointId::from("cp-2")));
    cards.observe(&resolved("r1", "cp-1")).expect("legal");
    assert_eq!(shown(&cards), Some(CheckpointId::from("cp-2")));
}

#[test]
fn tp_chk_07_a_checkpoint_shows_only_on_its_own_agents_card() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    running(&mut cards, "r2", "ranger");
    cards
        .observe(&opened("r1", "cp-1", "Sending", DETAIL, Risk::High))
        .expect("legal");
    assert!(cards.shows_checkpoint(&scout(), &CheckpointId::from("cp-1")));
    assert!(!cards.shows_checkpoint(&AgentId::from("ranger"), &CheckpointId::from("cp-1")));
    assert!(!cards.shows_checkpoint(&AgentId::from("nobody"), &CheckpointId::from("cp-1")));
    assert_eq!(
        cards
            .card(&AgentId::from("ranger"), 0)
            .and_then(|c| c.checkpoint),
        None
    );
}

#[test]
fn tp_chk_07_stopping_the_run_takes_the_checkpoint_off_the_card() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    cards
        .observe(&opened("r1", "cp-1", "Sending", DETAIL, Risk::High))
        .expect("legal");
    cards
        .observe(&ev(
            "r1",
            EventBody::RunCancelled(RunCancelled {
                by: CancelledBy::User,
            }),
        ))
        .expect("legal");
    let card = cards.card(&scout(), 0).expect("card");
    assert_eq!(card.status, CardStatus::Stopped);
    assert_eq!(card.checkpoint, None);
    assert!(!cards.shows_checkpoint(&scout(), &CheckpointId::from("cp-1")));
}

#[test]
fn tp_hover_08_the_approval_text_is_cleaned_and_capped() {
    let mut cards = RunCards::default();
    running(&mut cards, "r1", "scout");
    let sneaky = format!(
        "email.send to \u{202E}tset.elpmaxe@rekcatta\u{202C}\n{}",
        json!({"body": "x".repeat(5_000)})
    );
    cards
        .observe(&opened(
            "r1",
            "cp-1",
            &format!("Send\u{200B}ing {}", "a".repeat(500)),
            &sneaky,
            Risk::High,
        ))
        .expect("legal");
    let checkpoint = cards
        .card(&scout(), 0)
        .and_then(|c| c.checkpoint)
        .expect("checkpoint");
    assert!(
        !checkpoint.detail.as_str().contains('\u{202E}'),
        "bidi stripped"
    );
    assert!(checkpoint.detail.as_str().starts_with("email.send to tset"));
    assert!(checkpoint.detail.is_truncated());
    assert_eq!(
        checkpoint.detail.as_str().chars().count(),
        CHECKPOINT_DETAIL_MAX_CHARS
    );
    assert!(checkpoint.title.as_str().starts_with("Sending"));
    assert_eq!(
        checkpoint.title.as_str().chars().count(),
        CHECKPOINT_TITLE_MAX_CHARS
    );
    assert_eq!(
        (CHECKPOINT_TITLE_MAX_CHARS, CHECKPOINT_DETAIL_MAX_CHARS),
        (120, 2_000)
    );
}
