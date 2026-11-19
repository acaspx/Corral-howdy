//! Approve once per exact call (TP-POL-08, decided 2026-11-18; security model T4, approval
//! fatigue). Once the user approves a normal-risk call, the exact same call (same tool, same
//! arguments, in any key order) doesn't ask again in that run. Everything else still asks: a
//! different argument, a high or critical class, a call on a way out once the run is tainted, a
//! spend limit, an edited call, and every call from the loop detector's limit on.

use corral_core::domain::ActionClass;
use corral_policy::{Action, Approval, Gate, Policy, Profile, Scope, ToolCall, Verdict};
use serde_json::{Value, json};

fn call(class: ActionClass, tool: &str, args: Value) -> ToolCall {
    ToolCall {
        action: Action::new(class, tool),
        args,
    }
}

fn write(path: &str) -> ToolCall {
    call(
        ActionClass::WriteLocal,
        "fs.write",
        json!({"path": path, "text": "notes"}),
    )
}

fn gate() -> Gate {
    Gate::new(Policy::new(Profile::default()))
}

fn approve_once(gate: &Gate, call: ToolCall) {
    let Verdict::Ask(pending) = gate.check(call) else {
        panic!("asks first");
    };
    pending
        .approve(Approval::Click, Scope::Once)
        .expect("approved");
}

fn asks(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Ask(_))
}

fn allows(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Allow(_))
}

#[test]
fn tp_pol_08_an_approved_normal_risk_call_doesnt_ask_again_in_the_run() {
    let g = gate();
    approve_once(&g, write("notes.md"));
    let Verdict::Allow(token) = g.check(write("notes.md")) else {
        panic!("the same call is allowed");
    };
    assert_eq!(token.call(), &write("notes.md"));
}

#[test]
fn tp_pol_08_the_same_arguments_in_another_key_order_are_the_same_call() {
    let g = gate();
    approve_once(&g, write("notes.md"));
    let reordered = call(
        ActionClass::WriteLocal,
        "fs.write",
        serde_json::from_str(r#"{"text": "notes", "path": "notes.md"}"#).expect("json"),
    );
    assert!(allows(&g.check(reordered)));
}

#[test]
fn tp_pol_08_any_other_call_still_asks() {
    let g = gate();
    approve_once(&g, write("notes.md"));
    assert!(asks(&g.check(write("other.md"))), "another argument");
    assert!(
        asks(&g.check(call(
            ActionClass::WriteLocal,
            "fs.write",
            json!({"path": "notes.md", "text": "more notes"})
        ))),
        "another value"
    );
    assert!(
        asks(&g.check(call(
            ActionClass::WriteLocal,
            "fs.append",
            json!({"path": "notes.md", "text": "notes"})
        ))),
        "another tool with the same arguments"
    );
}

#[test]
fn tp_pol_08_high_and_critical_calls_ask_every_time() {
    let g = gate();
    let send = || {
        call(
            ActionClass::CommsSend,
            "email.send",
            json!({"to": "me@example.test"}),
        )
    };
    approve_once(&g, send());
    assert!(asks(&g.check(send())), "high");
    let pay = || {
        call(
            ActionClass::MoneySpend,
            "payment.send",
            json!({"amount": 5}),
        )
    };
    let Verdict::Ask(pending) = g.check(pay()) else {
        panic!("asks");
    };
    pending
        .approve(Approval::TouchId, Scope::Once)
        .expect("approved");
    assert!(asks(&g.check(pay())), "critical");
}

#[test]
fn tp_pol_08_the_loop_detector_still_asks_at_its_limit() {
    let g = Gate::new(Policy::new(
        Profile::from_toml("loop_limit = 3").expect("profile"),
    ));
    approve_once(&g, write("notes.md"));
    assert!(allows(&g.check(write("notes.md"))), "second");
    let Verdict::Ask(pending) = g.check(write("notes.md")) else {
        panic!("the third identical call asks");
    };
    assert_eq!(pending.repeated(), Some(3));
}

#[test]
fn tp_pol_08_a_denied_call_asks_again() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(write("notes.md")) else {
        panic!("asks");
    };
    pending.deny();
    assert!(asks(&g.check(write("notes.md"))));
}

#[test]
fn tp_pol_08_a_refused_gesture_records_nothing() {
    // Voice can approve normal risk, so use a high-risk call's refusal to show nothing leaks
    // across: then the normal-risk call still asks first.
    let g = gate();
    let Verdict::Ask(pending) = g.check(call(
        ActionClass::CommsSend,
        "email.send",
        json!({"to": "me@example.test"}),
    )) else {
        panic!("asks");
    };
    let refused = pending
        .approve(Approval::Voice, Scope::Once)
        .expect_err("voice can't approve high");
    drop(refused);
    assert!(asks(&g.check(write("notes.md"))));
}

#[test]
fn tp_pol_08_an_edited_call_is_for_that_call_only() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(write("draft.md")) else {
        panic!("asks");
    };
    pending
        .approve_edited(
            Approval::Click,
            json!({"path": "notes.md", "text": "notes"}),
            Some("notes.md".into()),
        )
        .expect("approved");
    assert!(asks(&g.check(write("notes.md"))), "the edited call");
    assert!(asks(&g.check(write("draft.md"))), "the original call");
}

#[test]
fn tp_pol_08_a_way_out_asks_again_once_the_run_is_tainted() {
    let g = gate();
    let get = || {
        call(
            ActionClass::NetReadUnlisted,
            "http.get",
            json!({"url": "https://unlisted.example/a"}),
        )
    };
    approve_once(&g, get());
    let Verdict::Allow(token) = g.check(get()) else {
        panic!("allowed before taint");
    };
    // What it read is untrusted, as the tool gateway marks it.
    token.taint("https://unlisted.example/a");
    let Verdict::Ask(pending) = g.check(get()) else {
        panic!("a tainted run asks before any way out");
    };
    assert!(
        pending.reason().contains("untrusted content"),
        "{}",
        pending.reason()
    );
    // Writing locally isn't a way out: an approved write is still remembered.
    approve_once(&g, write("notes.md"));
    assert!(allows(&g.check(write("notes.md"))));
}

#[test]
fn tp_pol_08_a_spend_limit_asks_every_time() {
    let g = gate();
    let soft = || {
        call(
            ActionClass::BudgetThreshold,
            "budget.soft_limit",
            json!({"spent_micro_usd": 600_000}),
        )
    };
    approve_once(&g, soft());
    assert!(asks(&g.check(soft())));
}

#[test]
fn tp_pol_08_a_new_run_remembers_nothing() {
    let first = gate();
    approve_once(&first, write("notes.md"));
    assert!(asks(&gate().check(write("notes.md"))));
}

#[test]
fn tp_pol_08_allow_for_this_run_still_covers_the_whole_class() {
    let g = gate();
    let Verdict::Ask(pending) = g.check(write("notes.md")) else {
        panic!("asks");
    };
    pending
        .approve(Approval::Click, Scope::ForRun)
        .expect("approved");
    assert!(allows(&g.check(write("other.md"))));
}
