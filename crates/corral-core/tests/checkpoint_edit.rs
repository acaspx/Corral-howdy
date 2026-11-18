//! Edit on the card (M2 item 4, TP-CHK-13..14, docs/07 §8.2): while a checkpoint waits, the core
//! says which of the call's arguments the card may change, one labeled field per top-level text or
//! number argument, and merges the card's changes into the agent's arguments itself. The card
//! sends back only what changed; read-only and nested values never cross back.

use corral_core::domain::{ActionClass, Risk};
use corral_core::event::*;
use corral_core::ids::{AgentId, CheckpointId, RunId, SessionId, SpanId, ToolCallId};
use corral_core::run_card::{
    CardField, EDIT_ARGS_MAX_BYTES, EDIT_MAX_FIELDS, EditError, FIXED_VALUE_MAX_CHARS, FieldKind,
    RunCards,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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

fn scout() -> AgentId {
    AgentId::from("scout")
}

fn cp() -> CheckpointId {
    CheckpointId::from("cp-1")
}

/// A run whose agent asked for `email.send` with `args`, waiting on checkpoint `cp-1`.
fn waiting_on(args: Value) -> RunCards {
    let mut cards = RunCards::default();
    for body in [
        EventBody::RunQueued(RunQueued {
            agent_id: scout(),
            session_id: SessionId::from("s1"),
            prompt: "Summarize the attached page.".into(),
        }),
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
        EventBody::ToolRequested(ToolRequested {
            tool_call_id: ToolCallId::from("call-1"),
            tool: "email.send".into(),
            action_class: ActionClass::CommsSend,
            args,
        }),
        opened("call-1"),
    ] {
        cards.observe(&ev(body)).expect("legal");
    }
    cards
}

fn opened(call: &str) -> EventBody {
    EventBody::CheckpointOpened(CheckpointOpened {
        checkpoint_id: cp(),
        tool_call_id: ToolCallId::from(call),
        action_class: ActionClass::CommsSend,
        risk: Risk::High,
        title: "Sending a message or email".into(),
        detail: "email.send".into(),
    })
}

fn fields(cards: &RunCards) -> Option<Vec<CardField>> {
    cards.card(&scout(), 0).expect("card").edit
}

fn field(key: &str, label: &str, kind: FieldKind, value: &str) -> CardField {
    CardField {
        key: key.into(),
        label: label.into(),
        kind,
        value: value.into(),
    }
}

fn changes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn email() -> Value {
    json!({
        "to": "attacker@example.test",
        "subject": "Your history",
        "body": "the history\nall of it",
        "retries": 3,
        "attachments": ["history.txt"],
        "urgent": true,
    })
}

// ---- TP-CHK-13: which arguments the card may change ----------------------------------------------

#[test]
fn tp_chk_13_text_and_number_arguments_become_labeled_fields_and_the_rest_are_read_only() {
    let cards = waiting_on(email());
    assert_eq!(
        fields(&cards).expect("editable"),
        [
            field(
                "attachments",
                "Attachments",
                FieldKind::Fixed,
                r#"["history.txt"]"#
            ),
            field("body", "Body", FieldKind::Text, "the history\nall of it"),
            field("retries", "Retries", FieldKind::Number, "3"),
            field("subject", "Subject", FieldKind::Text, "Your history"),
            field("to", "To", FieldKind::Text, "attacker@example.test"),
            field("urgent", "Urgent", FieldKind::Fixed, "true"),
        ]
    );
}

#[test]
fn tp_chk_13_labels_read_like_words() {
    let cards = waiting_on(json!({"file_path": "a.md", "max-bytes": 10, "dryRun": "no"}));
    let labels: Vec<String> = fields(&cards)
        .expect("editable")
        .into_iter()
        .map(|f| f.label)
        .collect();
    assert_eq!(labels, ["Dry run", "File path", "Max bytes"]);
}

#[test]
fn tp_chk_13_text_with_hidden_or_control_characters_is_read_only_and_shown_cleaned() {
    let cards = waiting_on(json!({
        "to": "me@example.test\u{202E}tset.elpmaxe@rekcatta",
        "body": "tab\there",
        "subject": "fine",
    }));
    let shown = fields(&cards).expect("subject is still editable");
    assert_eq!(
        shown,
        [
            field("body", "Body", FieldKind::Fixed, "tab here"),
            field("subject", "Subject", FieldKind::Text, "fine"),
            field(
                "to",
                "To",
                FieldKind::Fixed,
                "me@example.testtset.elpmaxe@rekcatta"
            ),
        ]
    );
}

#[test]
fn tp_chk_13_odd_keys_are_read_only_under_a_cleaned_label() {
    let long = "k".repeat(41);
    let cards = waiting_on(json!({
        "to\u{200B}": "x@example.test",
        long.clone(): "v",
        "ok": "v",
        "a\nb": "v",
        "n\u{200B}": 5,
    }));
    let shown = fields(&cards).expect("ok is editable");
    let kinds: Vec<(String, FieldKind)> = shown.iter().map(|f| (f.label.clone(), f.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("A b".to_owned(), FieldKind::Fixed),
            (format!("K{}", "k".repeat(39)), FieldKind::Fixed),
            ("N".to_owned(), FieldKind::Fixed),
            ("Ok".to_owned(), FieldKind::Text),
            ("To".to_owned(), FieldKind::Fixed),
        ]
    );
    assert_eq!(shown[1].key, long, "the key itself is kept to match on");
    let at_cap = "k".repeat(40);
    let shown = fields(&waiting_on(json!({ at_cap.clone(): "v" }))).expect("editable");
    assert_eq!(shown[0].kind, FieldKind::Text, "40 characters is fine");
}

#[test]
fn tp_chk_13_long_read_only_values_are_cut() {
    let cards = waiting_on(json!({"to": "a@example.test", "list": vec![1; 500]}));
    let list = &fields(&cards).expect("editable")[0];
    assert_eq!(list.kind, FieldKind::Fixed);
    assert_eq!(list.value.chars().count(), FIXED_VALUE_MAX_CHARS);
}

#[test]
fn tp_chk_13_no_fields_when_nothing_can_be_edited_on_a_card() {
    // Not an object.
    assert_eq!(fields(&waiting_on(json!(["a@example.test"]))), None);
    // Nothing editable in it.
    assert_eq!(
        fields(&waiting_on(json!({"cc": ["a"], "dry": false}))),
        None
    );
    assert_eq!(fields(&waiting_on(json!({}))), None);
    // Too big to edit here.
    let pad = "x".repeat(EDIT_ARGS_MAX_BYTES);
    assert_eq!(fields(&waiting_on(json!({"body": pad}))), None);
    // Too many arguments.
    let many: serde_json::Map<String, Value> = (0..=EDIT_MAX_FIELDS)
        .map(|i| (format!("k{i:02}"), json!("v")))
        .collect();
    assert_eq!(fields(&waiting_on(Value::Object(many))), None);
}

#[test]
fn tp_chk_13_the_size_and_count_limits_are_inclusive() {
    assert_eq!((EDIT_ARGS_MAX_BYTES, EDIT_MAX_FIELDS), (8 * 1024, 20));
    // `{"body":"…"}` is 11 bytes around the text.
    let pad = "x".repeat(EDIT_ARGS_MAX_BYTES - 11);
    assert!(fields(&waiting_on(json!({ "body": pad }))).is_some());
    let many: serde_json::Map<String, Value> = (0..EDIT_MAX_FIELDS)
        .map(|i| (format!("k{i:02}"), json!("v")))
        .collect();
    assert_eq!(
        fields(&waiting_on(Value::Object(many))).map(|f| f.len()),
        Some(EDIT_MAX_FIELDS)
    );
}

#[test]
fn tp_chk_13_a_checkpoint_without_a_tool_request_has_no_fields() {
    // A spend limit opens with no tool.requested before it.
    let mut cards = RunCards::default();
    for body in [
        EventBody::RunQueued(RunQueued {
            agent_id: scout(),
            session_id: SessionId::from("s1"),
            prompt: "p".into(),
        }),
        EventBody::RunStarted(RunStarted {
            adapter: "fake".into(),
        }),
        EventBody::ToolRequested(ToolRequested {
            tool_call_id: ToolCallId::from("call-1"),
            tool: "email.send".into(),
            action_class: ActionClass::CommsSend,
            args: json!({"to": "a@example.test"}),
        }),
        opened("budget-1"),
    ] {
        cards.observe(&ev(body)).expect("legal");
    }
    assert_eq!(fields(&cards), None, "the request was for another call");
}

#[test]
fn tp_chk_13_the_fields_go_when_the_checkpoint_is_answered() {
    let mut cards = waiting_on(email());
    cards
        .observe(&ev(EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id: cp(),
            resolution: Resolution::Denied { note: None },
        })))
        .expect("legal");
    assert_eq!(fields(&cards), None);
    assert_eq!(
        cards.edited_args(&scout(), &cp(), &changes(&[("to", "me@example.test")])),
        Err(EditError::NotShown)
    );
}

// ---- TP-CHK-14: the core merges the card's changes -----------------------------------------------

#[test]
fn tp_chk_14_changes_merge_into_the_agents_arguments() {
    let cards = waiting_on(email());
    let args = cards
        .edited_args(
            &scout(),
            &cp(),
            &changes(&[("to", "me@example.test"), ("retries", " 1 ")]),
        )
        .expect("merged");
    let mut want = email();
    want["to"] = json!("me@example.test");
    want["retries"] = json!(1);
    assert_eq!(args, want, "everything else is the agent's, unchanged");
}

#[test]
fn tp_chk_14_text_stays_text_even_when_it_looks_like_a_number() {
    let cards = waiting_on(email());
    let args = cards
        .edited_args(&scout(), &cp(), &changes(&[("subject", "42")]))
        .expect("merged");
    assert_eq!(args["subject"], json!("42"));
    let args = cards
        .edited_args(&scout(), &cp(), &changes(&[("body", "")]))
        .expect("empty text is text");
    assert_eq!(args["body"], json!(""));
}

#[test]
fn tp_chk_14_numbers_must_be_numbers() {
    let cards = waiting_on(email());
    for bad in ["", "three", "1e400", "NaN", "0x10", "1,000"] {
        assert_eq!(
            cards.edited_args(&scout(), &cp(), &changes(&[("retries", bad)])),
            Err(EditError::Number("retries".into())),
            "{bad:?}"
        );
    }
    let args = cards
        .edited_args(&scout(), &cp(), &changes(&[("retries", "-2.5")]))
        .expect("a decimal is a number");
    assert_eq!(args["retries"], json!(-2.5));
}

#[test]
fn tp_chk_14_only_editable_fields_can_change() {
    let cards = waiting_on(email());
    for key in ["attachments", "urgent", "bcc"] {
        assert_eq!(
            cards.edited_args(&scout(), &cp(), &changes(&[(key, "x")])),
            Err(EditError::Field(key.into())),
            "{key}"
        );
    }
}

#[test]
fn tp_chk_14_edited_text_must_be_plain() {
    let cards = waiting_on(email());
    for bad in ["me@example.test\u{202E}", "a\u{0007}b", "tab\there"] {
        assert_eq!(
            cards.edited_args(&scout(), &cp(), &changes(&[("to", bad)])),
            Err(EditError::Text("to".into())),
            "{bad:?}"
        );
    }
    assert!(
        cards
            .edited_args(&scout(), &cp(), &changes(&[("body", "line\nline")]))
            .is_ok(),
        "line breaks are plain"
    );
}

#[test]
fn tp_chk_14_an_edit_with_no_changes_or_for_another_card_is_refused() {
    let cards = waiting_on(email());
    assert_eq!(
        cards.edited_args(&scout(), &cp(), &BTreeMap::new()),
        Err(EditError::NoChanges)
    );
    assert_eq!(
        cards.edited_args(
            &AgentId::from("grok"),
            &cp(),
            &changes(&[("to", "me@example.test")])
        ),
        Err(EditError::NotShown)
    );
    assert_eq!(
        cards.edited_args(
            &scout(),
            &CheckpointId::from("cp-2"),
            &changes(&[("to", "me@example.test")])
        ),
        Err(EditError::NotShown)
    );
}

#[test]
fn tp_chk_14_a_checkpoint_with_no_fields_cant_be_edited() {
    let cards = waiting_on(json!({"cc": ["a"]}));
    assert_eq!(
        cards.edited_args(&scout(), &cp(), &changes(&[("cc", "b")])),
        Err(EditError::NotEditable)
    );
}

#[test]
fn tp_chk_14_the_errors_say_what_to_fix() {
    assert_eq!(
        EditError::Number("retries".into()).to_string(),
        "\"retries\" needs a number."
    );
    assert_eq!(
        EditError::Field("urgent".into()).to_string(),
        "\"urgent\" can't be changed here."
    );
    assert_eq!(
        EditError::Text("to".into()).to_string(),
        "\"to\" has hidden or control characters. Type it again."
    );
    assert_eq!(EditError::NoChanges.to_string(), "Nothing was changed.");
    assert_eq!(
        EditError::NotShown.to_string(),
        "That checkpoint isn't on this card now."
    );
    assert_eq!(
        EditError::NotEditable.to_string(),
        "This can't be edited on the card."
    );
}
