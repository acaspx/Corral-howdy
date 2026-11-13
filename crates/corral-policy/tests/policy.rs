//! The policy engine (M2 items 1 and 2; docs/01 §5, security model T1, T3, T21, T28): a pure,
//! deny-by-default `decide(policy, action, run)` that returns Allow, Ask or Deny for every agent
//! action. TP-POL-01..04, 09, 11, 12.

use corral_core::domain::{ActionClass, Risk};
use corral_policy::{
    Action, Approval, Ceilings, Decision, OnTimeout, Policy, PolicyError, Profile, Rule, RunGrants,
    can_approve, default_rule, risk_of,
};
use std::time::Duration;

const ALL: [ActionClass; 13] = [
    ActionClass::ReadLocal,
    ActionClass::ReadLocalOutside,
    ActionClass::WriteLocal,
    ActionClass::DeleteLocal,
    ActionClass::ExecShell,
    ActionClass::NetRead,
    ActionClass::NetReadUnlisted,
    ActionClass::NetWrite,
    ActionClass::CommsSend,
    ActionClass::MoneySpend,
    ActionClass::ProviderTools,
    ActionClass::BudgetThreshold,
    ActionClass::Unknown,
];

fn action(class: ActionClass) -> Action {
    Action::new(class, "tool.name")
}

fn to(class: ActionClass, destination: &str) -> Action {
    Action::new(class, "tool.name").to(destination)
}

fn default_policy() -> Policy {
    Policy::new(Profile::default())
}

fn is_ask(d: &Decision, risk: Risk) -> bool {
    matches!(d, Decision::Ask { risk: r, .. } if *r == risk)
}

// ---- TP-POL-01: every class under the default profile --------------------------------------

#[test]
fn tp_pol_01_default_profile_decides_every_class_as_the_design_says() {
    let policy = default_policy();
    let run = RunGrants::default();
    let expected = [
        (ActionClass::ReadLocal, "allow", None),
        (ActionClass::ReadLocalOutside, "ask", Some(Risk::Normal)),
        (ActionClass::WriteLocal, "ask", Some(Risk::Normal)),
        (ActionClass::DeleteLocal, "ask", Some(Risk::High)),
        (ActionClass::ExecShell, "ask", Some(Risk::High)),
        (ActionClass::NetRead, "allow", None),
        (ActionClass::NetReadUnlisted, "ask", Some(Risk::Normal)),
        (ActionClass::NetWrite, "ask", Some(Risk::High)),
        (ActionClass::CommsSend, "ask", Some(Risk::High)),
        (ActionClass::MoneySpend, "ask", Some(Risk::Critical)),
        (ActionClass::ProviderTools, "deny", None),
        (ActionClass::BudgetThreshold, "ask", Some(Risk::Normal)),
        (ActionClass::Unknown, "deny", None),
    ];
    assert_eq!(expected.len(), ALL.len(), "every class is in the table");
    for (class, kind, risk) in expected {
        let d = policy.decide(&action(class), &run);
        match (kind, &d) {
            ("allow", Decision::Allow) => {}
            ("deny", Decision::Deny { .. }) => {}
            ("ask", Decision::Ask { risk: r, reason }) => {
                assert_eq!(Some(*r), risk, "{class:?}");
                assert!(!reason.is_empty(), "{class:?} says why it asks");
            }
            _ => panic!("{class:?}: expected {kind}, got {d:?}"),
        }
    }
}

#[test]
fn tp_pol_01_risk_follows_the_class_and_deleting_outside_the_sandbox_is_critical() {
    assert_eq!(risk_of(&action(ActionClass::DeleteLocal)), Risk::High);
    assert_eq!(
        risk_of(&action(ActionClass::DeleteLocal).outside_sandbox()),
        Risk::Critical
    );
    assert_eq!(risk_of(&action(ActionClass::MoneySpend)), Risk::Critical);
    assert_eq!(risk_of(&action(ActionClass::CommsSend)), Risk::High);
    assert_eq!(risk_of(&action(ActionClass::WriteLocal)), Risk::Normal);
    let d = default_policy().decide(
        &action(ActionClass::DeleteLocal).outside_sandbox(),
        &RunGrants::default(),
    );
    assert!(is_ask(&d, Risk::Critical), "{d:?}");
}

#[test]
fn tp_pol_01_default_rules_are_listed_per_class() {
    for class in ALL {
        let rule = default_rule(class);
        let d = default_policy().decide(&action(class), &RunGrants::default());
        let matches = matches!(
            (rule, &d),
            (Rule::Allow, Decision::Allow)
                | (Rule::Ask, Decision::Ask { .. })
                | (Rule::Deny, Decision::Deny { .. })
        );
        assert!(matches, "{class:?}: {rule:?} vs {d:?}");
    }
}

// ---- TP-POL-02: profiles widen and narrow, never past the ceilings ------------------------------

const RESEARCHER: &str = r#"
sandbox_roots = ["~/Corral/Scout"]
net_allow = ["*.wikipedia.org", "arxiv.org"]
rules = [
  { class = "write.local", decision = "allow" },
  { class = "net.write", decision = "deny" },
  { class = "comms.send", decision = "deny" },
]
checkpoint_timeout = "30m"
on_timeout = "deny"
"#;

#[test]
fn tp_pol_02_a_profile_from_toml_widens_and_narrows() {
    let profile = Profile::from_toml(RESEARCHER).expect("the design's example parses");
    assert_eq!(profile.sandbox_roots, ["~/Corral/Scout"]);
    assert_eq!(profile.net_allow, ["*.wikipedia.org", "arxiv.org"]);
    assert_eq!(profile.checkpoint_timeout, Duration::from_secs(30 * 60));
    assert_eq!(profile.on_timeout, OnTimeout::Deny);
    let policy = Policy::new(profile);
    let run = RunGrants::default();
    assert_eq!(
        policy.decide(&action(ActionClass::WriteLocal), &run),
        Decision::Allow,
        "widened"
    );
    assert!(matches!(
        policy.decide(&action(ActionClass::NetWrite), &run),
        Decision::Deny { .. }
    ));
    assert!(matches!(
        policy.decide(&action(ActionClass::CommsSend), &run),
        Decision::Deny { .. }
    ));
    assert!(
        is_ask(
            &policy.decide(&action(ActionClass::ExecShell), &run),
            Risk::High
        ),
        "untouched classes keep their defaults"
    );
}

#[test]
fn tp_pol_02_the_ceilings_cap_what_a_profile_can_allow() {
    let profile = Profile::from_toml(
        r#"rules = [
          { class = "money.spend", decision = "allow" },
          { class = "delete.local", decision = "allow" },
          { class = "exec.shell", decision = "allow" },
        ]"#,
    )
    .expect("parses");
    let policy = Policy::new(profile);
    let run = RunGrants::default();
    assert!(is_ask(
        &policy.decide(&action(ActionClass::MoneySpend), &run),
        Risk::Critical
    ));
    assert!(is_ask(
        &policy.decide(&action(ActionClass::DeleteLocal), &run),
        Risk::High
    ));
    assert!(is_ask(
        &policy.decide(&action(ActionClass::ExecShell), &run),
        Risk::High
    ));

    // Tighter ceilings tighten further; a ceiling of Deny blocks the class outright.
    let strict = Ceilings::default().cap(ActionClass::WriteLocal, Rule::Deny);
    let policy = Policy::with_ceilings(Profile::from_toml(RESEARCHER).expect("parses"), strict);
    assert!(matches!(
        policy.decide(&action(ActionClass::WriteLocal), &run),
        Decision::Deny { .. }
    ));
}

#[test]
fn tp_pol_02_profiles_are_parsed_strictly() {
    for (bad, why) in [
        (r#"rulez = []"#, "unknown key"),
        (
            r#"rules = [{ class = "write.local", decision = "yes" }]"#,
            "bad decision",
        ),
        (
            r#"rules = [{ class = "write.anywhere", decision = "allow" }]"#,
            "unknown class",
        ),
        (
            r#"rules = [{ class = "write.local", decision = "allow" }, { class = "write.local", decision = "deny" }]"#,
            "duplicate class",
        ),
        (r#"checkpoint_timeout = "soon""#, "bad duration"),
        (r#"checkpoint_timeout = "0m""#, "zero duration"),
        (
            r#"on_timeout = "approve""#,
            "on_timeout can only deny or cancel",
        ),
        (
            r#"rules = [{ class = "write.local", decision = "allow", note = "x" }]"#,
            "unknown rule key",
        ),
    ] {
        assert!(Profile::from_toml(bad).is_err(), "{why}: {bad}");
    }
    assert!(matches!(
        Profile::from_toml(
            r#"rules = [{ class = "write.local", decision = "allow" }, { class = "write.local", decision = "deny" }]"#
        ),
        Err(PolicyError::DuplicateRule(ActionClass::WriteLocal))
    ));
    let empty = Profile::from_toml("").expect("an empty profile is the defaults");
    assert_eq!(empty, Profile::default());
    assert_eq!(empty.on_timeout, OnTimeout::Deny);
    assert_eq!(empty.checkpoint_timeout, Duration::from_secs(30 * 60));
}

// ---- TP-POL-03: unknown is always denied ---------------------------------------------------------

#[test]
fn tp_pol_03_unknown_is_denied_and_no_profile_can_allow_it() {
    let d = default_policy().decide(&action(ActionClass::Unknown), &RunGrants::default());
    assert!(matches!(d, Decision::Deny { .. }), "{d:?}");
    assert_eq!(
        Profile::from_toml(r#"rules = [{ class = "unknown", decision = "allow" }]"#),
        Err(PolicyError::UnknownCanOnlyBeDenied)
    );
    assert_eq!(
        Profile::from_toml(r#"rules = [{ class = "unknown", decision = "ask" }]"#),
        Err(PolicyError::UnknownCanOnlyBeDenied)
    );
    // Even a grant can't open it.
    let mut run = RunGrants::default();
    run.allow_for_run(&action(ActionClass::Unknown));
    assert!(matches!(
        default_policy().decide(&action(ActionClass::Unknown), &run),
        Decision::Deny { .. }
    ));
}

// ---- TP-POL-04: allow-for-run lives in the run, never in the profile -----------------------------

#[test]
fn tp_pol_04_allow_for_run_covers_the_run_and_nothing_else() {
    let policy = default_policy();
    let mut run = RunGrants::default();
    let write = action(ActionClass::WriteLocal);
    assert!(is_ask(&policy.decide(&write, &run), Risk::Normal));
    run.allow_for_run(&write);
    assert_eq!(policy.decide(&write, &run), Decision::Allow);
    // Normal-risk grants cover the class, whatever the destination.
    assert_eq!(
        policy.decide(&to(ActionClass::WriteLocal, "/elsewhere"), &run),
        Decision::Allow
    );
    // A new run starts with no grants, and the policy itself didn't change.
    assert!(is_ask(
        &policy.decide(&write, &RunGrants::default()),
        Risk::Normal
    ));
    assert_eq!(policy, default_policy());
    // Other classes are untouched.
    assert!(is_ask(
        &policy.decide(&action(ActionClass::ReadLocalOutside), &run),
        Risk::Normal
    ));
}

#[test]
fn tp_pol_04_a_grant_never_beats_a_deny() {
    let policy = Policy::new(Profile::from_toml(RESEARCHER).expect("parses"));
    let mut run = RunGrants::default();
    let send = to(ActionClass::CommsSend, "friend@example.test");
    run.allow_for_run(&send);
    assert!(matches!(policy.decide(&send, &run), Decision::Deny { .. }));
}

// ---- TP-POL-09: the gesture each risk needs ------------------------------------------------------

#[test]
fn tp_pol_09_risk_sets_the_gesture_that_can_approve() {
    use Approval::*;
    let table = [
        (Risk::Normal, Click, true),
        (Risk::Normal, Keyboard, true),
        (Risk::Normal, Voice, true),
        (Risk::Normal, TouchId, true),
        (Risk::High, Click, true),
        (Risk::High, Keyboard, true),
        (Risk::High, Voice, false),
        (Risk::High, TouchId, true),
        (Risk::Critical, Click, false),
        (Risk::Critical, Keyboard, false),
        (Risk::Critical, Voice, false),
        (Risk::Critical, TouchId, true),
    ];
    for (risk, how, ok) in table {
        assert_eq!(can_approve(risk, how), ok, "{risk:?} by {how:?}");
    }
}

// ---- TP-POL-11: high-risk grants are pinned to their destination ---------------------------------

#[test]
fn tp_pol_11_allow_for_run_on_high_risk_covers_only_that_destination() {
    let policy = default_policy();
    let mut run = RunGrants::default();
    let friend = to(ActionClass::CommsSend, "friend@example.test");
    run.allow_for_run(&friend);
    assert_eq!(policy.decide(&friend, &run), Decision::Allow);
    let attacker = to(ActionClass::CommsSend, "attacker@example.test");
    let d = policy.decide(&attacker, &run);
    assert!(is_ask(&d, Risk::High), "a new recipient asks again: {d:?}");
    // Destinations compare exactly, ignoring only ASCII case.
    assert_eq!(
        policy.decide(&to(ActionClass::CommsSend, "Friend@Example.TEST"), &run),
        Decision::Allow
    );
    assert!(is_ask(
        &policy.decide(
            &to(ActionClass::CommsSend, "friend@example.test.evil"),
            &run
        ),
        Risk::High
    ));
    // A high-risk action with no destination can't use a grant.
    assert!(is_ask(
        &policy.decide(&action(ActionClass::CommsSend), &run),
        Risk::High
    ));
    let mut blank = RunGrants::default();
    blank.allow_for_run(&action(ActionClass::NetWrite));
    assert!(is_ask(
        &policy.decide(&action(ActionClass::NetWrite), &blank),
        Risk::High
    ));
}

#[test]
fn tp_pol_11_critical_grants_are_pinned_too() {
    let policy = default_policy();
    let mut run = RunGrants::default();
    run.allow_for_run(&to(ActionClass::MoneySpend, "store.example"));
    assert_eq!(
        policy.decide(&to(ActionClass::MoneySpend, "store.example"), &run),
        Decision::Allow
    );
    assert!(is_ask(
        &policy.decide(&to(ActionClass::MoneySpend, "other.example"), &run),
        Risk::Critical
    ));
}

// ---- TP-POL-12: a tainted run asks again before sending anything out -----------------------------

#[test]
fn tp_pol_12_on_a_tainted_run_grants_on_ways_out_stop_applying() {
    let policy = default_policy();
    let mut run = RunGrants::default();
    let post = to(ActionClass::NetWrite, "api.example");
    let send = to(ActionClass::CommsSend, "friend@example.test");
    let fetch = to(ActionClass::NetReadUnlisted, "news.example");
    let write = action(ActionClass::WriteLocal);
    for a in [&post, &send, &fetch, &write] {
        run.allow_for_run(a);
    }
    for a in [&post, &send, &fetch, &write] {
        assert_eq!(
            policy.decide(a, &run),
            Decision::Allow,
            "{a:?} before taint"
        );
    }

    run.taint("https://news.example/page");
    for a in [&post, &send, &fetch] {
        let Decision::Ask { reason, .. } = policy.decide(a, &run) else {
            panic!("{a:?} must ask on a tainted run");
        };
        assert!(
            reason.contains("https://news.example/page"),
            "the checkpoint names the untrusted source: {reason}"
        );
    }
    assert_eq!(
        policy.decide(&write, &run),
        Decision::Allow,
        "grants on classes that can't send data out still apply"
    );

    // Taint never clears within the run, and the first source is the one named.
    run.taint("https://other.example");
    assert_eq!(run.tainted_by(), Some("https://news.example/page"));
    let mut later = run.clone();
    later.allow_for_run(&post);
    assert!(matches!(policy.decide(&post, &later), Decision::Ask { .. }));
}

#[test]
fn tp_pol_12_taint_doesnt_change_what_the_profile_already_decides() {
    let mut run = RunGrants::default();
    run.taint("mcp:untrusted-server");
    let policy = default_policy();
    assert_eq!(
        policy.decide(&action(ActionClass::ReadLocal), &run),
        Decision::Allow
    );
    assert!(is_ask(
        &policy.decide(&to(ActionClass::NetWrite, "api.example"), &run),
        Risk::High
    ));
    assert!(matches!(
        policy.decide(&action(ActionClass::Unknown), &run),
        Decision::Deny { .. }
    ));
}

// ---- Decisions read plainly ------------------------------------------------------------------------

#[test]
fn decisions_say_why_in_plain_words() {
    let run = RunGrants::default();
    let Decision::Deny { reason } = default_policy().decide(&action(ActionClass::Unknown), &run)
    else {
        panic!("deny");
    };
    assert_eq!(
        reason,
        "Corral doesn't know what this action does, so it's blocked."
    );
    let Decision::Ask { reason, .. } =
        default_policy().decide(&to(ActionClass::CommsSend, "friend@example.test"), &run)
    else {
        panic!("ask");
    };
    assert_eq!(reason, "Sending a message or email needs your approval.");
}

// ---- Mutation gaps (cargo-mutants) ----------------------------------------------------------------

#[test]
fn checkpoint_timeouts_take_seconds_minutes_and_hours_up_to_a_week() {
    let timeout = |t: &str| {
        Profile::from_toml(&format!("checkpoint_timeout = \"{t}\"")).map(|p| p.checkpoint_timeout)
    };
    assert_eq!(timeout("90s"), Ok(Duration::from_secs(90)));
    assert_eq!(timeout("5m"), Ok(Duration::from_secs(300)));
    assert_eq!(timeout("2h"), Ok(Duration::from_secs(7200)));
    assert_eq!(
        timeout("168h"),
        Ok(Duration::from_secs(168 * 3600)),
        "a week is the most"
    );
    assert_eq!(timeout("604800s"), Ok(Duration::from_secs(604_800)));
    for bad in ["604801s", "169h", "0s", "1d", "m", "", "-5m", "5 m"] {
        assert!(
            matches!(timeout(bad), Err(PolicyError::BadDuration(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn a_policy_keeps_its_profile_for_the_tools_that_need_it() {
    let profile = Profile::from_toml(RESEARCHER).expect("parses");
    let policy = Policy::new(profile.clone());
    assert_eq!(policy.profile(), &profile);
    assert_eq!(policy.profile().sandbox_roots, ["~/Corral/Scout"]);
}
