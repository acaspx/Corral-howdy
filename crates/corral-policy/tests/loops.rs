//! The loop detector (M2 item 7, EV-COST-02, security model T9): a run's gate counts identical
//! calls (the same tool with the same arguments, whatever order their keys come in). From the
//! profile's `loop_limit`-th one on (5 by default), each asks the user before it runs, even when
//! the policy allows it outright or the user allowed the class for the run. A denied call stays
//! denied. A new run starts counting from zero.

use corral_core::domain::{ActionClass, Risk};
use corral_policy::{
    Action, Approval, DEFAULT_LOOP_LIMIT, Gate, Policy, PolicyError, Profile, Scope, ToolCall,
    Verdict,
};
use serde_json::{Value, json};

fn gate(profile: &str) -> Gate {
    Gate::new(Policy::new(Profile::from_toml(profile).expect("profile")))
}

fn read(path: &str) -> ToolCall {
    ToolCall {
        action: Action::new(ActionClass::ReadLocal, "fs.read"),
        args: json!({ "path": path }),
    }
}

fn kind(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Allow(_) => "allow",
        Verdict::Ask(_) => "ask",
        Verdict::Deny { .. } => "deny",
    }
}

#[test]
fn tp_loop_01_the_nth_identical_call_asks_even_when_the_policy_allows_it() {
    assert_eq!(DEFAULT_LOOP_LIMIT, 5);
    let g = gate("");
    let first_four: Vec<&str> = (0..4).map(|_| kind(&g.check(read("notes.md")))).collect();
    assert_eq!(first_four, ["allow"; 4], "read.local is allowed");
    let Verdict::Ask(pending) = g.check(read("notes.md")) else {
        panic!("the fifth identical call asks");
    };
    assert_eq!(pending.repeated(), Some(5));
    assert_eq!(pending.risk(), Risk::Normal);
    assert_eq!(
        pending.reason(),
        "fs.read has run with the same arguments 5 times in this run. Corral asks before it runs again, in case the agent is stuck in a loop."
    );
    assert_eq!(
        kind(&g.check(read("notes.md"))),
        "ask",
        "each identical call after that asks too"
    );
}

#[test]
fn tp_loop_02_only_the_same_tool_with_the_same_arguments_counts_whatever_the_key_order() {
    let g = gate("");
    for i in 0..10 {
        assert_eq!(
            kind(&g.check(read(&format!("file-{i}.md")))),
            "allow",
            "different args"
        );
    }
    let other_tool = |args: Value| ToolCall {
        action: Action::new(ActionClass::ReadLocal, "fs.stat"),
        args,
    };
    for _ in 0..4 {
        assert_eq!(kind(&g.check(read("same.md"))), "allow");
    }
    assert_eq!(
        kind(&g.check(other_tool(json!({ "path": "same.md" })))),
        "allow",
        "another tool with the same arguments is another call"
    );
    let parse = |text: &str| serde_json::from_str::<Value>(text).expect("json");
    let in_order = parse(r#"{"a": 1, "b": {"x": [1, {"p": 2, "q": 1}], "y": 2}}"#);
    let reordered = parse(r#"{"b": {"y": 2, "x": [1, {"q": 1, "p": 2}]}, "a": 1}"#);
    let g = gate("loop_limit = 2");
    let call = |args: Value| ToolCall {
        action: Action::new(ActionClass::ReadLocal, "fs.read"),
        args,
    };
    assert_eq!(kind(&g.check(call(in_order))), "allow");
    assert_eq!(
        kind(&g.check(call(reordered))),
        "ask",
        "reordering the keys doesn't dodge the count"
    );
    assert_eq!(
        kind(&g.check(call(json!({ "a": 1, "b": 3 })))),
        "allow",
        "a different value is a different call"
    );
}

#[test]
fn tp_loop_03_an_approval_for_the_run_doesnt_skip_the_loop_check() {
    let g = gate(r#"loop_limit = 3"#);
    let send = || ToolCall {
        action: Action::new(ActionClass::WriteLocal, "fs.write").to("notes.md"),
        args: json!({ "path": "notes.md", "content": "again" }),
    };
    let Verdict::Ask(first) = g.check(send()) else {
        panic!("write.local asks");
    };
    assert_eq!(first.repeated(), None, "not a loop yet");
    first
        .approve(Approval::Click, Scope::ForRun)
        .expect("allowed for the run");
    assert_eq!(
        kind(&g.check(send())),
        "allow",
        "the grant covers the second"
    );
    let Verdict::Ask(third) = g.check(send()) else {
        panic!("the third identical call asks despite the grant");
    };
    assert_eq!(third.repeated(), Some(3));
}

#[test]
fn tp_loop_04_a_denied_call_stays_denied_and_an_asking_one_says_why() {
    let g = gate(
        r#"loop_limit = 2
rules = [{ class = "comms.send", decision = "deny" }]"#,
    );
    let email = || ToolCall {
        action: Action::new(ActionClass::CommsSend, "email.send").to("a@example.test"),
        args: json!({ "to": "a@example.test" }),
    };
    for _ in 0..3 {
        assert_eq!(kind(&g.check(email())), "deny", "never upgraded to an ask");
    }
    let unknown = || ToolCall {
        action: Action::new(ActionClass::Unknown, "mystery"),
        args: json!({}),
    };
    for _ in 0..3 {
        assert_eq!(kind(&g.check(unknown())), "deny");
    }
    let shell = || ToolCall {
        action: Action::new(ActionClass::ExecShell, "shell.exec"),
        args: json!({ "cmd": "ls" }),
    };
    let Verdict::Ask(once) = g.check(shell()) else {
        panic!("exec.shell asks");
    };
    assert_eq!(once.repeated(), None);
    assert_eq!(once.risk(), Risk::High);
    let Verdict::Ask(twice) = g.check(shell()) else {
        panic!("asks again");
    };
    assert_eq!(twice.repeated(), Some(2));
    assert_eq!(twice.risk(), Risk::High, "the class's risk stays");
    assert!(
        twice.reason().contains("stuck in a loop"),
        "{}",
        twice.reason()
    );
}

#[test]
fn tp_loop_05_a_new_run_starts_counting_from_zero() {
    let policy = Policy::new(Profile::from_toml("loop_limit = 2").expect("profile"));
    let run1 = Gate::new(policy.clone());
    assert_eq!(kind(&run1.check(read("a.md"))), "allow");
    assert_eq!(kind(&run1.check(read("a.md"))), "ask");
    let run2 = Gate::new(policy);
    assert_eq!(kind(&run2.check(read("a.md"))), "allow");
}

#[test]
fn tp_loop_06_the_profile_sets_the_limit_within_reason() {
    assert_eq!(Profile::default().loop_limit, DEFAULT_LOOP_LIMIT);
    assert_eq!(
        Profile::from_toml("loop_limit = 2").expect("ok").loop_limit,
        2
    );
    assert_eq!(
        Profile::from_toml("loop_limit = 1000")
            .expect("ok")
            .loop_limit,
        1_000
    );
    for bad in [0, 1, 1_001] {
        assert_eq!(
            Profile::from_toml(&format!("loop_limit = {bad}")),
            Err(PolicyError::BadLoopLimit(bad)),
            "{bad}"
        );
    }
    assert!(matches!(
        Profile::from_toml(r#"loop_limit = "five""#),
        Err(PolicyError::Parse(_))
    ));
    assert!(matches!(
        Profile::from_toml("loop_limit = -3"),
        Err(PolicyError::Parse(_))
    ));
    assert_eq!(
        PolicyError::BadLoopLimit(1).to_string(),
        "loop_limit must be from 2 to 1,000 (got 1)"
    );
}
