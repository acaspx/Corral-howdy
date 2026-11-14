//! The policy gate (M2 item 3; T2): one per run. Every tool call goes through `Gate::check`, and
//! the only way to get an `AllowToken`, which the tool gateway needs to run anything, is an Allow
//! decision or an approved checkpoint. TP-POL-10 (the policy half), TP-GATE-01..03.

use corral_core::domain::{ActionClass, Risk};
use corral_policy::{Action, Approval, Gate, Policy, Profile, Refused, Scope, ToolCall, Verdict};
use serde_json::json;

fn call(class: ActionClass, tool: &str, destination: Option<&str>) -> ToolCall {
    let mut action = Action::new(class, tool);
    if let Some(d) = destination {
        action = action.to(d);
    }
    ToolCall {
        action,
        args: json!({"to": destination}),
    }
}

fn gate() -> Gate {
    Gate::new(Policy::new(Profile::default()))
}

#[test]
fn tp_pol_10_an_allowed_call_gets_a_token_for_exactly_that_call() {
    let g = gate();
    let read = call(ActionClass::ReadLocal, "fs.read", None);
    let Verdict::Allow(token) = g.check(read.clone()) else {
        panic!("reads in the sandbox are allowed");
    };
    assert_eq!(token.call(), &read);
}

#[test]
fn tp_pol_10_a_denied_call_gets_no_token() {
    let g = gate();
    let Verdict::Deny { reason } = g.check(call(ActionClass::Unknown, "mystery", None)) else {
        panic!("unknown is denied");
    };
    assert!(!reason.is_empty());
}

#[test]
fn tp_gate_01_an_ask_waits_for_an_approval_with_the_right_gesture() {
    let g = gate();
    let send = call(
        ActionClass::CommsSend,
        "email.send",
        Some("friend@example.test"),
    );
    let Verdict::Ask(pending) = g.check(send.clone()) else {
        panic!("sending asks");
    };
    assert_eq!(pending.risk(), Risk::High);
    assert_eq!(pending.call(), &send);
    assert_eq!(
        pending.reason(),
        "Sending a message or email needs your approval."
    );

    // Voice can't approve a high-risk action; the checkpoint stays open.
    let Err(Refused { pending, .. }) = pending.approve(Approval::Voice, Scope::Once) else {
        panic!("voice can't approve high risk");
    };
    let token = pending
        .approve(Approval::Click, Scope::Once)
        .expect("a click can");
    assert_eq!(token.call(), &send);

    // Once means once: the same call asks again.
    assert!(matches!(g.check(send), Verdict::Ask(_)));
}

#[test]
fn tp_gate_01_critical_needs_touch_id() {
    let g = gate();
    let buy = call(ActionClass::MoneySpend, "shop.buy", Some("store.example"));
    let Verdict::Ask(pending) = g.check(buy) else {
        panic!("money asks");
    };
    assert_eq!(pending.risk(), Risk::Critical);
    let Err(Refused { pending, reason }) = pending.approve(Approval::Click, Scope::Once) else {
        panic!("a click can't approve spending");
    };
    assert_eq!(reason, "This needs Touch ID.");
    assert!(pending.approve(Approval::TouchId, Scope::Once).is_ok());
}

#[test]
fn tp_gate_02_allow_for_run_applies_to_later_calls_in_this_run_only() {
    let g = gate();
    let friend = call(
        ActionClass::CommsSend,
        "email.send",
        Some("friend@example.test"),
    );
    let Verdict::Ask(pending) = g.check(friend.clone()) else {
        panic!("asks");
    };
    pending
        .approve(Approval::Click, Scope::ForRun)
        .expect("approved for the run");
    assert!(matches!(g.check(friend.clone()), Verdict::Allow(_)));
    // Pinned to the destination (T21).
    assert!(matches!(
        g.check(call(
            ActionClass::CommsSend,
            "email.send",
            Some("attacker@example.test")
        )),
        Verdict::Ask(_)
    ));
    // A new run has its own gate and starts with nothing.
    assert!(matches!(gate().check(friend), Verdict::Ask(_)));
}

#[test]
fn tp_gate_02_denying_a_checkpoint_gives_no_token_and_grants_nothing() {
    let g = gate();
    let post = call(ActionClass::NetWrite, "http.post", Some("api.example"));
    let Verdict::Ask(pending) = g.check(post.clone()) else {
        panic!("asks");
    };
    let denied = pending.deny();
    assert_eq!(denied.action.tool, "http.post");
    assert!(matches!(g.check(post), Verdict::Ask(_)));
}

#[test]
fn tp_gate_03_a_token_can_taint_its_run_and_ways_out_ask_again() {
    let g = gate();
    let post = call(ActionClass::NetWrite, "http.post", Some("api.example"));
    let Verdict::Ask(pending) = g.check(post.clone()) else {
        panic!("asks");
    };
    pending
        .approve(Approval::Click, Scope::ForRun)
        .expect("approved");
    assert!(matches!(g.check(post.clone()), Verdict::Allow(_)));

    // A read of untrusted content (the gateway does this after running the tool).
    let Verdict::Allow(read) =
        g.check(call(ActionClass::NetRead, "http.get", Some("news.example")))
    else {
        panic!("allowlisted reads are allowed");
    };
    read.taint("https://news.example/page");
    assert_eq!(g.tainted_by().as_deref(), Some("https://news.example/page"));

    let Verdict::Ask(again) = g.check(post) else {
        panic!("a way out asks again on a tainted run");
    };
    assert!(again.reason().contains("https://news.example/page"));
}
