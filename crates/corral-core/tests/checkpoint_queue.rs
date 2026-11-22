//! Several checkpoints waiting on one card (M4 item 4, docs/07 §8.2, TP-CHK-19): the card steps
//! through them ("1 of 3"), so an answer may be for any of them, never for one that isn't waiting
//! on that agent's card. The oldest still shows first.

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::run_card::{EditError, RunCards};
use serde_json::json;
use std::collections::BTreeMap;

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

fn scout() -> AgentId {
    AgentId::from("scout")
}

fn cp(id: &str) -> CheckpointId {
    CheckpointId::from(id)
}

fn start(cards: &mut RunCards, run: &str, agent: &str) {
    for body in [
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from(agent),
            session_id: SessionId::from(format!("s-{run}").as_str()),
            prompt: "Tidy my inbox".into(),
        }),
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
    ] {
        cards.observe(&ev(run, body)).expect("legal");
    }
}

/// A call to `to` waiting at checkpoint `id`, with `risk`.
fn wait(cards: &mut RunCards, run: &str, id: &str, to: &str, risk: Risk) {
    let call = format!("call-{id}");
    for body in [
        EventBody::ToolRequested(ToolRequested {
            tool_call_id: ToolCallId::from(call.as_str()),
            tool: "email.send".into(),
            action_class: ActionClass::CommsSend,
            args: json!({ "to": to }),
        }),
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: cp(id),
            tool_call_id: ToolCallId::from(call.as_str()),
            action_class: ActionClass::CommsSend,
            risk,
            title: format!("Email {to}"),
            detail: to.into(),
        }),
    ] {
        cards.observe(&ev(run, body)).expect("legal");
    }
}

fn resolve(cards: &mut RunCards, run: &str, id: &str) {
    cards
        .observe(&ev(
            run,
            EventBody::CheckpointResolved(CheckpointResolved {
                checkpoint_id: cp(id),
                resolution: Resolution::Approved,
            }),
        ))
        .expect("legal");
}

fn three() -> RunCards {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    wait(&mut cards, "r1", "cp-1", "a@example.test", Risk::High);
    wait(&mut cards, "r1", "cp-2", "b@example.test", Risk::Critical);
    wait(&mut cards, "r1", "cp-3", "c@example.test", Risk::Normal);
    cards
}

fn ids(cards: &RunCards, agent: &AgentId) -> Vec<String> {
    cards
        .waiting(agent)
        .iter()
        .map(|(checkpoint, _)| checkpoint.id.as_str().to_owned())
        .collect()
}

#[test]
fn tp_chk_19_every_waiting_checkpoint_is_listed_oldest_first_with_its_fields() {
    let cards = three();
    assert_eq!(ids(&cards, &scout()), ["cp-1", "cp-2", "cp-3"]);
    let waiting = cards.waiting(&scout());
    let second = waiting[1].1.as_ref().expect("its own fields");
    assert_eq!(second[0].value, "b@example.test");
    assert_eq!(
        cards
            .card(&scout(), 0)
            .and_then(|c| c.checkpoint)
            .map(|c| c.id),
        Some(cp("cp-1")),
        "the card still shows the oldest first"
    );
    assert!(cards.waiting(&AgentId::from("ranger")).is_empty());
}

#[test]
fn tp_chk_19_an_answer_may_be_for_any_waiting_checkpoint_on_that_card_only() {
    let mut cards = three();
    for id in ["cp-1", "cp-2", "cp-3"] {
        assert!(cards.waits_on(&scout(), &cp(id)), "{id}");
    }
    assert!(!cards.waits_on(&scout(), &cp("cp-9")));
    assert!(!cards.waits_on(&AgentId::from("ranger"), &cp("cp-2")));
    resolve(&mut cards, "r1", "cp-2");
    assert!(!cards.waits_on(&scout(), &cp("cp-2")), "answered");
    assert_eq!(ids(&cards, &scout()), ["cp-1", "cp-3"]);

    let mut other = RunCards::default();
    start(&mut other, "r2", "ranger");
    wait(&mut other, "r2", "cp-r", "r@example.test", Risk::High);
    assert!(
        !other.waits_on(&scout(), &cp("cp-r")),
        "another agent's checkpoint"
    );
}

#[test]
fn tp_chk_19_a_run_that_ends_waits_on_nothing() {
    let mut cards = three();
    cards
        .observe(&ev(
            "r1",
            EventBody::RunCancelled(RunCancelled {
                by: CancelledBy::User,
            }),
        ))
        .expect("legal");
    assert!(cards.waiting(&scout()).is_empty());
    assert!(!cards.waits_on(&scout(), &cp("cp-1")));
}

#[test]
fn tp_chk_19_any_waiting_call_can_be_edited_with_its_own_fields() {
    let cards = three();
    let changes = BTreeMap::from([("to".to_owned(), "me@example.test".to_owned())]);
    let edited = cards
        .edited_args(&scout(), &cp("cp-3"), &changes)
        .expect("cp-3 waits");
    assert_eq!(edited, json!({ "to": "me@example.test" }));
    assert_eq!(
        cards.edited_args(&scout(), &cp("cp-9"), &changes),
        Err(EditError::NotShown)
    );
}

#[test]
fn tp_chk_19_the_card_can_show_any_waiting_checkpoint_for_touch_id() {
    let cards = three();
    let card = cards
        .card_showing(&scout(), &cp("cp-2"), 0)
        .expect("cp-2 waits");
    let shown = card.checkpoint.expect("shown");
    assert_eq!((shown.id, shown.risk), (cp("cp-2"), Risk::Critical));
    assert_eq!(card.edit.expect("its fields")[0].value, "b@example.test");
    assert!(cards.card_showing(&scout(), &cp("cp-9"), 0).is_none());
}
