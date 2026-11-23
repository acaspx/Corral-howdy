//! The checkpoint notification (M4 item 4, docs/01 "Notifications", docs/07 §8.2, TP-CHK-22,
//! TP-CHK-23): each waiting checkpoint gets one macOS notification, with a fresh single-use nonce.
//! Only a normal-risk one offers Approve inline; high and critical say to approve on the card. A
//! click counts only with the nonce it carried, once, and only while its checkpoint waits.

use corral_core::checkpoint_note::{
    BODY_MAX_CHARS, MAX_NOTES, NoteAnswer, NoteClick, NoteDesk, NoteOffer, NoteTrouble, Notice,
    TITLE_MAX_CHARS, offers,
};
use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::run_card::RunCards;
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

fn wait_with(cards: &mut RunCards, run: &str, id: &str, risk: Risk, title: &str, detail: &str) {
    let call = format!("call-{id}");
    for body in [
        EventBody::ToolRequested(ToolRequested {
            tool_call_id: ToolCallId::from(call.as_str()),
            tool: "email.send".into(),
            action_class: ActionClass::CommsSend,
            args: json!({ "to": detail }),
        }),
        EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: cp(id),
            tool_call_id: ToolCallId::from(call.as_str()),
            action_class: ActionClass::CommsSend,
            risk,
            title: title.into(),
            detail: detail.into(),
        }),
    ] {
        cards.observe(&ev(run, body)).expect("legal");
    }
}

fn wait(cards: &mut RunCards, run: &str, id: &str, risk: Risk) {
    wait_with(
        cards,
        run,
        id,
        risk,
        "Sending a message or email",
        "to a@example.test",
    );
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

/// Nonces n-1, n-2, … in the order they're minted.
struct Mint(u32);

impl Mint {
    fn next(&mut self) -> String {
        self.0 += 1;
        format!("n-{}", self.0)
    }
}

fn names(agent: &AgentId) -> String {
    match agent.as_str() {
        "scout" => "Scout".into(),
        other => other.into(),
    }
}

fn observe(desk: &mut NoteDesk, cards: &RunCards, mint: &mut Mint) -> Vec<Notice> {
    desk.observe(cards, &names, &mut || mint.next())
}

fn one(risk: Risk) -> (RunCards, NoteDesk, Notice) {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    wait(&mut cards, "r1", "cp-1", risk);
    let mut desk = NoteDesk::default();
    let mut notices = observe(&mut desk, &cards, &mut Mint(0));
    assert_eq!(notices.len(), 1);
    let notice = notices.remove(0);
    (cards, desk, notice)
}

// ---- TP-CHK-22: one notification per waiting checkpoint, with the core's buttons -------------

#[test]
fn tp_chk_22_only_a_normal_risk_checkpoint_offers_approve_inline() {
    assert_eq!(
        offers(Risk::Normal),
        [NoteOffer::Approve, NoteOffer::Deny, NoteOffer::Open]
    );
    assert_eq!(offers(Risk::High), [NoteOffer::Deny, NoteOffer::Open]);
    assert_eq!(offers(Risk::Critical), [NoteOffer::Deny, NoteOffer::Open]);
}

#[test]
fn tp_chk_22_a_checkpoint_opening_posts_one_notification_with_a_fresh_nonce() {
    let (cards, mut desk, notice) = one(Risk::Normal);
    assert_eq!(notice.checkpoint, cp("cp-1"));
    assert_eq!(notice.agent, scout());
    assert_eq!(notice.nonce, "n-1");
    assert_eq!(notice.title, "Scout needs your approval");
    assert_eq!(notice.body, "Sending a message or email: to a@example.test");
    assert_eq!(notice.offers, offers(Risk::Normal));
    assert!(
        observe(&mut desk, &cards, &mut Mint(10)).is_empty(),
        "once per checkpoint"
    );
}

#[test]
fn tp_chk_22_high_and_critical_say_to_approve_on_the_card() {
    let (_, _, high) = one(Risk::High);
    assert_eq!(high.offers, offers(Risk::High));
    assert!(
        high.body.ends_with(" Approve it on its card."),
        "{}",
        high.body
    );
    let (_, _, critical) = one(Risk::Critical);
    assert_eq!(critical.offers, offers(Risk::Critical));
    assert!(
        critical
            .body
            .ends_with(" Approve it on its card with Touch ID."),
        "{}",
        critical.body
    );
}

#[test]
fn tp_chk_22_several_waiting_each_get_their_own_nonce_oldest_first() {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    start(&mut cards, "r2", "ranger");
    wait(&mut cards, "r1", "cp-1", Risk::Normal);
    wait(&mut cards, "r1", "cp-2", Risk::High);
    wait(&mut cards, "r2", "cp-3", Risk::Normal);
    let mut desk = NoteDesk::default();
    let notices = observe(&mut desk, &cards, &mut Mint(0));
    let got: Vec<(&str, &str, &str)> = notices
        .iter()
        .map(|n| (n.agent.as_str(), n.checkpoint.as_str(), n.nonce.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("ranger", "cp-3", "n-1"),
            ("scout", "cp-1", "n-2"),
            ("scout", "cp-2", "n-3"),
        ]
    );
    assert_eq!(notices[0].title, "ranger needs your approval");

    wait(&mut cards, "r1", "cp-4", Risk::Normal);
    let later = observe(&mut desk, &cards, &mut Mint(3));
    assert_eq!(later.len(), 1, "only the new one");
    assert_eq!(later[0].checkpoint, cp("cp-4"));
}

#[test]
fn tp_chk_22_the_text_is_cleaned_cut_and_on_one_line() {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    let long = format!(
        "to\nbob@example.test\u{202e} {}",
        "x".repeat(2 * BODY_MAX_CHARS)
    );
    wait_with(
        &mut cards,
        "r1",
        "cp-1",
        Risk::Normal,
        "Sending\tmail",
        &long,
    );
    let mut desk = NoteDesk::default();
    let long_name = |_: &AgentId| format!("Sc\nout{}", "y".repeat(2 * TITLE_MAX_CHARS));
    let notice = desk
        .observe(&cards, &long_name, &mut || "n-1".to_owned())
        .remove(0);
    assert!(
        notice
            .body
            .starts_with("Sending mail: to bob@example.test x"),
        "{}",
        notice.body
    );
    assert!(notice.body.chars().count() <= BODY_MAX_CHARS);
    assert!(notice.title.chars().count() <= TITLE_MAX_CHARS);
    assert!(notice.title.starts_with("Sc out"), "{}", notice.title);
    for text in [&notice.title, &notice.body] {
        assert!(!text.contains(['\n', '\t', '\u{202e}']), "{text:?}");
    }
}

#[test]
fn tp_chk_22_the_desk_is_bounded() {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    for i in 0..MAX_NOTES + 5 {
        wait(&mut cards, "r1", &format!("cp-{i:03}"), Risk::Normal);
    }
    let mut desk = NoteDesk::default();
    let notices = observe(&mut desk, &cards, &mut Mint(0));
    assert_eq!(notices.len(), MAX_NOTES, "the rest wait on the card");
    resolve(&mut cards, "r1", "cp-000");
    let more = observe(&mut desk, &cards, &mut Mint(100));
    assert_eq!(more.len(), 1, "room for one more");
    assert_eq!(more[0].checkpoint, cp(&format!("cp-{MAX_NOTES:03}")));
}

// ---- TP-CHK-23: a click counts with its nonce, once, while its checkpoint waits -------------

#[test]
fn tp_chk_23_approve_from_a_normal_risk_notification_answers_it_once() {
    let (_, mut desk, notice) = one(Risk::Normal);
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
        NoteAnswer::Approve {
            agent: scout(),
            checkpoint: cp("cp-1")
        }
    );
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
        NoteAnswer::Stale,
        "single use"
    );
}

#[test]
fn tp_chk_23_approve_is_refused_for_high_and_critical_whatever_swift_sends() {
    for risk in [Risk::High, Risk::Critical] {
        let (_, mut desk, notice) = one(risk);
        assert_eq!(
            desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
            NoteAnswer::Refused {
                agent: scout(),
                checkpoint: cp("cp-1")
            },
            "{risk:?}"
        );
    }
}

#[test]
fn tp_chk_23_deny_open_and_dismiss() {
    let (_, mut desk, notice) = one(Risk::High);
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Deny),
        NoteAnswer::Deny {
            agent: scout(),
            checkpoint: cp("cp-1")
        }
    );
    let (_, mut desk, notice) = one(Risk::Critical);
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Open),
        NoteAnswer::Open {
            agent: scout(),
            checkpoint: cp("cp-1")
        }
    );
    let (_, mut desk, notice) = one(Risk::Normal);
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Dismiss),
        NoteAnswer::Dismissed
    );
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
        NoteAnswer::Stale,
        "a dismissed notification is gone"
    );
}

#[test]
fn tp_chk_23_a_wrong_nonce_or_checkpoint_is_stale_and_spends_nothing() {
    let (_, mut desk, notice) = one(Risk::Normal);
    assert_eq!(
        desk.click(&notice.checkpoint, "n-9", NoteClick::Approve),
        NoteAnswer::Stale
    );
    assert_eq!(
        desk.click(&cp("cp-9"), &notice.nonce, NoteClick::Approve),
        NoteAnswer::Stale
    );
    assert!(
        matches!(
            desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
            NoteAnswer::Approve { .. }
        ),
        "the real one still works"
    );
}

#[test]
fn tp_chk_23_a_checkpoint_answered_elsewhere_is_stale() {
    let (mut cards, mut desk, notice) = one(Risk::Normal);
    resolve(&mut cards, "r1", "cp-1");
    assert!(observe(&mut desk, &cards, &mut Mint(1)).is_empty());
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Approve),
        NoteAnswer::Stale
    );
}

#[test]
fn tp_chk_23_a_run_that_ends_takes_its_notifications_with_it() {
    let (mut cards, mut desk, notice) = one(Risk::Normal);
    cards
        .observe(&ev(
            "r1",
            EventBody::RunCancelled(RunCancelled {
                by: CancelledBy::User,
            }),
        ))
        .expect("legal");
    assert!(observe(&mut desk, &cards, &mut Mint(1)).is_empty());
    assert_eq!(
        desk.click(&notice.checkpoint, &notice.nonce, NoteClick::Deny),
        NoteAnswer::Stale
    );
}

#[test]
fn tp_chk_23_not_allowed_notifications_stop_for_the_session() {
    let mut cards = RunCards::default();
    start(&mut cards, "r1", "scout");
    wait(&mut cards, "r1", "cp-1", Risk::Normal);
    let mut desk = NoteDesk::default();
    assert_eq!(observe(&mut desk, &cards, &mut Mint(0)).len(), 1);
    desk.failed(&cp("cp-1"), NoteTrouble::Failed);
    assert!(!desk.muted(), "one failure isn't a no");
    wait(&mut cards, "r1", "cp-2", Risk::Normal);
    assert_eq!(observe(&mut desk, &cards, &mut Mint(1)).len(), 1);
    desk.failed(&cp("cp-2"), NoteTrouble::NotAuthorized);
    assert!(desk.muted());
    wait(&mut cards, "r1", "cp-3", Risk::Normal);
    assert!(
        observe(&mut desk, &cards, &mut Mint(2)).is_empty(),
        "the card still shows it"
    );
}
