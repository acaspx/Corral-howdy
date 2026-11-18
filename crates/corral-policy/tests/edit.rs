//! Edit, then approve (M2 item 4, TP-POL-06, the policy half). The user changes a waiting call's
//! arguments and approves the changed call. The tool and its class can't change, so the risk and
//! the gesture it needs stay the same; the destination is the one the tool reads from the edited
//! arguments; and an edit is an approval for that one call only, never a grant for the run.

use corral_core::domain::{ActionClass, Risk};
use corral_policy::{
    Action, Approval, EditRefusal, EditRefused, Gate, Policy, Profile, ToolCall, Verdict,
};
use serde_json::json;

fn send(to: &str) -> ToolCall {
    ToolCall {
        action: Action::new(ActionClass::CommsSend, "email.send").to(to),
        args: json!({"to": to, "body": "hi"}),
    }
}

fn gate() -> Gate {
    Gate::new(Policy::new(Profile::default()))
}

#[test]
fn tp_pol_06_an_edit_approves_exactly_the_edited_call() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(send("attacker@example.test")) else {
        panic!("sending asks");
    };
    assert!(pending.editable());
    let token = pending
        .approve_edited(
            Approval::Click,
            json!({"to": "me@example.test", "body": "hi"}),
            Some("me@example.test".into()),
        )
        .expect("a click can approve an edited send");
    assert_eq!(token.call(), &send("me@example.test"));
    assert_eq!(
        token.call().action.tool,
        "email.send",
        "the tool can't change"
    );
    assert_eq!(token.call().action.class, ActionClass::CommsSend);
}

#[test]
fn tp_pol_06_an_edit_can_drop_the_destination() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(send("attacker@example.test")) else {
        panic!("asks");
    };
    let token = pending
        .approve_edited(Approval::Keyboard, json!({"body": "hi"}), None)
        .expect("approved");
    assert_eq!(token.call().action.destination, None);
    assert_eq!(token.call().args, json!({"body": "hi"}));
}

#[test]
fn tp_pol_06_an_edit_is_for_that_call_only() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(send("attacker@example.test")) else {
        panic!("asks");
    };
    pending
        .approve_edited(
            Approval::Click,
            json!({"to": "me@example.test"}),
            Some("me@example.test".into()),
        )
        .expect("approved");
    // No grant: neither the edited destination nor the original one is allowed now.
    assert!(matches!(g.check(send("me@example.test")), Verdict::Ask(_)));
    assert!(matches!(
        g.check(send("attacker@example.test")),
        Verdict::Ask(_)
    ));
}

#[test]
fn tp_pol_06_an_edit_needs_the_gesture_an_approval_would() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(send("attacker@example.test")) else {
        panic!("asks");
    };
    let Err(EditRefused { pending, why }) = pending.approve_edited(
        Approval::Voice,
        json!({"to": "me@example.test"}),
        Some("me@example.test".into()),
    ) else {
        panic!("voice can't approve a high-risk edit");
    };
    assert_eq!(why, EditRefusal::Gesture);
    // Still open, with the original call.
    assert_eq!(pending.risk(), Risk::High);
    assert_eq!(pending.call(), &send("attacker@example.test"));
    assert!(
        pending
            .approve_edited(Approval::Click, json!({}), None)
            .is_ok()
    );

    let buy = ToolCall {
        action: Action::new(ActionClass::MoneySpend, "payment.send").to("store.example"),
        args: json!({"amount": 500}),
    };
    let Verdict::Ask(pending) = g.check(buy) else {
        panic!("money asks");
    };
    let Err(EditRefused { pending, why }) =
        pending.approve_edited(Approval::Click, json!({"amount": 5}), None)
    else {
        panic!("a click can't approve spending, edited or not");
    };
    assert_eq!(why, EditRefusal::Gesture);
    let token = pending
        .approve_edited(Approval::TouchId, json!({"amount": 5}), None)
        .expect("Touch ID can");
    assert_eq!(token.call().args, json!({"amount": 5}));
}

#[test]
fn tp_pol_06_a_spend_limit_checkpoint_cant_be_edited() {
    let g = gate();
    let soft = ToolCall {
        action: Action::new(ActionClass::BudgetThreshold, "budget.soft_limit"),
        args: json!({"spent_micro_usd": 600_000}),
    };
    let Verdict::Ask(pending) = g.check(soft.clone()) else {
        panic!("the soft limit asks");
    };
    assert!(!pending.editable());
    let Err(EditRefused { pending, why }) =
        pending.approve_edited(Approval::Click, json!({"spent_micro_usd": 0}), None)
    else {
        panic!("the numbers are the core's, not the user's to change");
    };
    assert_eq!(why, EditRefusal::NotEditable);
    assert_eq!(pending.call(), &soft, "still open, unchanged");
}

#[test]
fn tp_pol_06_a_repeated_call_can_be_edited_and_says_why_it_asked() {
    let g = Gate::new(Policy::new(
        Profile::from_toml(
            r#"loop_limit = 2
rules = [{ class = "write.local", decision = "allow" }]"#,
        )
        .expect("profile"),
    ));
    let write = |path: &str| ToolCall {
        action: Action::new(ActionClass::WriteLocal, "fs.write").to(path),
        args: json!({"path": path}),
    };
    assert!(matches!(g.check(write("a.md")), Verdict::Allow(_)));
    let Verdict::Ask(pending) = g.check(write("a.md")) else {
        panic!("the second identical write asks");
    };
    assert_eq!(pending.repeated(), Some(2));
    let token = pending
        .approve_edited(
            Approval::Click,
            json!({"path": "b.md"}),
            Some("b.md".into()),
        )
        .expect("approved");
    assert_eq!(token.call(), &write("b.md"));
}
