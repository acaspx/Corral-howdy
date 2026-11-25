//! The policy editor's core (M4 item 6, docs/01 §5, security model T7; TP-PED-01..04): each
//! agent's effective decision per action class and where it comes from, a profile written back
//! as TOML that reads the same, and an edit checked against the ceilings before anything is
//! saved. The editor shows and sends; the policy decides.

use corral_core::domain::ActionClass;
use corral_policy::{
    Action, CLASSES, Ceilings, Decision, MAX_NET_ALLOW, OnTimeout, Policy, PolicyError, Profile,
    ProfileEdit, Rule, RuleSource, RunGrants, default_rule, editable,
};
use std::time::Duration;

fn default_policy() -> Policy {
    Policy::new(Profile::default())
}

fn policy(toml: &str) -> Policy {
    Policy::new(Profile::from_toml(toml).expect("valid profile"))
}

/// The edit that changes nothing about `policy`.
fn same(policy: &Policy) -> ProfileEdit {
    ProfileEdit::from(policy.profile())
}

// ---- TP-PED-01: the effective decision per class, and why --------------------------------

#[test]
fn tp_ped_01_every_class_is_listed_once_in_display_order() {
    assert_eq!(CLASSES.len(), 13);
    for (i, class) in CLASSES.iter().enumerate() {
        assert!(!CLASSES[..i].contains(class), "{class:?} twice");
    }
    assert_eq!(CLASSES[0], ActionClass::ReadLocal);
    assert_eq!(CLASSES[12], ActionClass::Unknown, "unknown last");
}

#[test]
fn tp_ped_01_a_default_agent_shows_the_defaults() {
    let p = default_policy();
    for class in CLASSES {
        let e = p.effective(class);
        assert_eq!(e.class, class);
        assert_eq!(e.default, default_rule(class));
        let want = match class {
            ActionClass::Unknown => RuleSource::Fixed,
            ActionClass::ReadLocal
            | ActionClass::ReadLocalOutside
            | ActionClass::WriteLocal
            | ActionClass::DeleteLocal
            | ActionClass::ExecShell
            | ActionClass::NetRead
            | ActionClass::NetReadUnlisted
            | ActionClass::NetWrite
            | ActionClass::CommsSend
            | ActionClass::MoneySpend
            | ActionClass::ProviderTools
            | ActionClass::BudgetThreshold => RuleSource::Default,
        };
        assert_eq!(e.source, want, "{class:?}");
        assert_eq!(e.rule, default_rule(class).min(e.ceiling), "{class:?}");
    }
    assert_eq!(p.effective(ActionClass::Unknown).rule, Rule::Deny);
}

#[test]
fn tp_ped_01_a_profile_rule_shows_as_the_agents_own_and_a_capped_one_says_so() {
    let p = policy(
        "[[rules]]\nclass = \"write.local\"\ndecision = \"allow\"\n\
         [[rules]]\nclass = \"comms.send\"\ndecision = \"deny\"\n\
         [[rules]]\nclass = \"money.spend\"\ndecision = \"allow\"\n",
    );
    let write = p.effective(ActionClass::WriteLocal);
    assert_eq!(
        (write.rule, write.source),
        (Rule::Allow, RuleSource::Profile)
    );
    let send = p.effective(ActionClass::CommsSend);
    assert_eq!((send.rule, send.source), (Rule::Deny, RuleSource::Profile));
    let money = p.effective(ActionClass::MoneySpend);
    assert_eq!(
        (money.rule, money.source, money.ceiling),
        (Rule::Ask, RuleSource::Ceiling, Rule::Ask),
        "the ceiling wins, and the editor says why"
    );
    assert!(
        matches!(
            p.decide(
                &Action::new(ActionClass::MoneySpend, "pay"),
                &RunGrants::default()
            ),
            Decision::Ask { .. }
        ),
        "what it shows is what it decides"
    );
}

#[test]
fn tp_ped_01_the_editor_changes_neither_unknown_nor_provider_tools() {
    for class in CLASSES {
        let want = !matches!(class, ActionClass::Unknown | ActionClass::ProviderTools);
        assert_eq!(editable(class), want, "{class:?}");
        assert_eq!(
            default_policy().effective(class).editable,
            want,
            "{class:?}"
        );
    }
}

// ---- TP-PED-02: written back as TOML, it reads the same ----------------------------------

#[test]
fn tp_ped_02_a_profile_written_out_reads_back_the_same() {
    for text in [
        "",
        "net_allow = [\"*.wikipedia.org\", \"example.com\"]\n\
         checkpoint_timeout = \"90s\"\non_timeout = \"cancel_run\"\nloop_limit = 12\n\
         [[rules]]\nclass = \"net.write\"\ndecision = \"deny\"\n\
         [[rules]]\nclass = \"write.local\"\ndecision = \"allow\"\n\
         [[rules]]\nclass = \"provider_tools\"\ndecision = \"allow\"\n",
        "checkpoint_timeout = \"2h\"\n",
        "checkpoint_timeout = \"45m\"\n",
    ] {
        let profile = Profile::from_toml(text).expect("valid");
        let written = profile.to_toml().expect("writes");
        assert_eq!(
            Profile::from_toml(&written).expect("reads back"),
            profile,
            "{written}"
        );
        assert_eq!(
            written,
            Profile::from_toml(&written)
                .expect("again")
                .to_toml()
                .expect("writes"),
            "stable"
        );
    }
}

#[test]
fn tp_ped_02_rules_are_written_in_display_order() {
    let profile = Profile::from_toml(
        "[[rules]]\nclass = \"net.write\"\ndecision = \"deny\"\n\
         [[rules]]\nclass = \"read.local\"\ndecision = \"ask\"\n",
    )
    .expect("valid");
    let written = profile.to_toml().expect("writes");
    let read = written.find("read.local").expect("read");
    let net = written.find("net.write").expect("net");
    assert!(read < net, "{written}");
}

// ---- TP-PED-03: an edit is checked before it's saved --------------------------------------

#[test]
fn tp_ped_03_an_edit_sets_and_clears_rules_and_the_settings() {
    let start = policy("[[rules]]\nclass = \"comms.send\"\ndecision = \"deny\"\n");
    let mut edit = same(&start);
    edit.rules = vec![
        (ActionClass::WriteLocal, Some(Rule::Allow)),
        (ActionClass::CommsSend, None),
    ];
    edit.net_allow = vec![
        " Example.COM ".into(),
        "*.wikipedia.org".into(),
        "example.com".into(),
    ];
    edit.checkpoint_timeout = "10m".into();
    edit.on_timeout = OnTimeout::CancelRun;
    edit.loop_limit = 8;
    let next = start.edited(&edit).expect("valid edit");
    let p = next.profile();
    assert_eq!(p.rule(ActionClass::WriteLocal), Some(Rule::Allow));
    assert_eq!(p.rule(ActionClass::CommsSend), None, "back to the default");
    assert_eq!(
        p.net_allow,
        ["example.com", "*.wikipedia.org"],
        "trimmed, lowercased, once"
    );
    assert_eq!(p.checkpoint_timeout, Duration::from_secs(600));
    assert_eq!(p.on_timeout, OnTimeout::CancelRun);
    assert_eq!(p.loop_limit, 8);
    assert_eq!(
        next.effective(ActionClass::CommsSend).source,
        RuleSource::Default
    );
}

#[test]
fn tp_ped_03_an_edit_keeps_what_it_doesnt_name() {
    let start = policy(
        "[[rules]]\nclass = \"provider_tools\"\ndecision = \"allow\"\n\
         [[rules]]\nclass = \"net.write\"\ndecision = \"deny\"\n",
    );
    let mut edit = same(&start);
    edit.rules = vec![(ActionClass::WriteLocal, Some(Rule::Allow))];
    let next = start.edited(&edit).expect("valid");
    assert_eq!(
        next.profile().rule(ActionClass::ProviderTools),
        Some(Rule::Allow),
        "provider tools stay as the agent editor set them"
    );
    assert_eq!(next.profile().rule(ActionClass::NetWrite), Some(Rule::Deny));
    assert_eq!(same(&start), ProfileEdit::from(start.profile()));
    assert_eq!(start.edited(&same(&start)).expect("no change"), start);
}

#[test]
fn tp_ped_03_a_rule_above_its_ceiling_is_refused_not_capped() {
    let mut edit = same(&default_policy());
    edit.rules = vec![(ActionClass::MoneySpend, Some(Rule::Allow))];
    assert_eq!(
        default_policy().edited(&edit),
        Err(PolicyError::AboveCeiling {
            class: ActionClass::MoneySpend,
            ceiling: Rule::Ask
        })
    );
    let lower = Policy::with_ceilings(
        Profile::default(),
        Ceilings::default().cap(ActionClass::WriteLocal, Rule::Ask),
    );
    let mut edit = same(&lower);
    edit.rules = vec![(ActionClass::WriteLocal, Some(Rule::Allow))];
    assert!(matches!(
        lower.edited(&edit),
        Err(PolicyError::AboveCeiling {
            class: ActionClass::WriteLocal,
            ..
        })
    ));
    edit.rules = vec![(ActionClass::WriteLocal, Some(Rule::Deny))];
    assert!(lower.edited(&edit).is_ok(), "stricter is always fine");
}

#[test]
fn tp_ped_03_unknown_and_provider_tools_cant_be_edited_here() {
    for class in [ActionClass::Unknown, ActionClass::ProviderTools] {
        let mut edit = same(&default_policy());
        edit.rules = vec![(class, Some(Rule::Deny))];
        assert_eq!(
            default_policy().edited(&edit),
            Err(PolicyError::NotEditable(class))
        );
    }
}

#[test]
fn tp_ped_03_domains_are_plain_hostnames_or_one_leading_wildcard() {
    let try_one = |domain: &str| {
        let mut edit = same(&default_policy());
        edit.net_allow = vec![domain.into()];
        default_policy()
            .edited(&edit)
            .map(|p| p.profile().net_allow.clone())
    };
    for good in [
        "example.com",
        "*.wikipedia.org",
        "docs.rs",
        "xn--bcher-kva.example",
        "a-b.example.co.uk",
    ] {
        assert_eq!(try_one(good), Ok(vec![good.to_owned()]), "{good}");
    }
    for bad in [
        "",
        "   ",
        "*",
        "*.com",
        "localhost",
        "https://example.com",
        "example.com/path",
        "example.com:443",
        "ex*ample.com",
        "*.*.example.com",
        "bücher.example",
        "a b.example",
        "-example.com",
        "example-.com",
        "example..com",
        ".example.com",
        "example.com.",
        "192.168.1.1",
    ] {
        assert!(
            matches!(try_one(bad), Err(PolicyError::BadDomain(_))),
            "{bad:?} should be refused"
        );
    }
    let long = format!("{}.example.com", "a".repeat(64));
    assert!(
        matches!(try_one(&long), Err(PolicyError::BadDomain(_))),
        "a label over 63"
    );
    let mut many = same(&default_policy());
    many.net_allow = (0..=MAX_NET_ALLOW)
        .map(|i| format!("s{i}.example.com"))
        .collect();
    assert_eq!(
        default_policy().edited(&many),
        Err(PolicyError::TooManyDomains(MAX_NET_ALLOW + 1))
    );
}

#[test]
fn tp_ped_03_the_timeout_and_loop_limit_keep_their_ranges() {
    for (text, ok) in [
        ("30m", true),
        ("1s", true),
        ("168h", true),
        ("0s", false),
        ("169h", false),
        ("soon", false),
    ] {
        let mut edit = same(&default_policy());
        edit.checkpoint_timeout = text.into();
        assert_eq!(default_policy().edited(&edit).is_ok(), ok, "{text}");
    }
    for (n, ok) in [(2, true), (1_000, true), (1, false), (1_001, false)] {
        let mut edit = same(&default_policy());
        edit.loop_limit = n;
        assert_eq!(default_policy().edited(&edit).is_ok(), ok, "{n}");
    }
}

// ---- TP-PED-04: the edited policy decides with the new rules -------------------------------

#[test]
fn tp_ped_04_what_the_editor_saved_is_what_the_agent_gets() {
    let mut edit = same(&default_policy());
    edit.rules = vec![
        (ActionClass::WriteLocal, Some(Rule::Allow)),
        (ActionClass::NetWrite, Some(Rule::Deny)),
    ];
    let next = default_policy().edited(&edit).expect("valid");
    let decide = |class| next.decide(&Action::new(class, "tool"), &RunGrants::default());
    assert_eq!(decide(ActionClass::WriteLocal), Decision::Allow);
    assert!(matches!(
        decide(ActionClass::NetWrite),
        Decision::Deny { .. }
    ));
    let saved = Profile::from_toml(&next.profile().to_toml().expect("writes")).expect("reads back");
    assert_eq!(&saved, next.profile());
}

#[test]
fn tp_ped_03_exactly_the_most_domains_is_fine() {
    let mut edit = same(&default_policy());
    edit.net_allow = (0..MAX_NET_ALLOW)
        .map(|i| format!("s{i}.example.com"))
        .collect();
    assert_eq!(
        default_policy()
            .edited(&edit)
            .map(|p| p.profile().net_allow.len()),
        Ok(MAX_NET_ALLOW)
    );
}

// ---- TP-PED-05: a saved profile is checked as the editor would have ----------------------

#[test]
fn tp_ped_05_a_profile_the_editor_could_save_is_accepted() {
    let saved = default_policy()
        .edited(&ProfileEdit {
            rules: vec![
                (ActionClass::MoneySpend, Some(Rule::Ask)),
                (ActionClass::WriteLocal, Some(Rule::Allow)),
            ],
            net_allow: vec!["*.wikipedia.org".into()],
            checkpoint_timeout: "5m".into(),
            on_timeout: OnTimeout::Deny,
            loop_limit: 4,
        })
        .expect("valid");
    let text = saved.profile().to_toml().expect("writes");
    let read = Profile::from_toml(&text).expect("reads");
    assert_eq!(Policy::checked(read), Ok(saved), "at its ceiling is fine");
    let with_tools =
        Profile::from_toml("[[rules]]\nclass = \"provider_tools\"\ndecision = \"allow\"\n")
            .expect("valid");
    assert!(
        Policy::checked(with_tools).is_ok(),
        "the agent editor's switch"
    );
}

#[test]
fn tp_ped_05_a_profile_the_editor_couldnt_save_is_refused() {
    let read = |text: &str| Policy::checked(Profile::from_toml(text).expect("parses"));
    assert_eq!(
        read("[[rules]]\nclass = \"money.spend\"\ndecision = \"allow\"\n"),
        Err(PolicyError::AboveCeiling {
            class: ActionClass::MoneySpend,
            ceiling: Rule::Ask
        })
    );
    assert!(
        read("[[rules]]\nclass = \"exec.shell\"\ndecision = \"deny\"\n").is_ok(),
        "stricter"
    );
    assert_eq!(
        read("net_allow = [\"Example.com\"]\n"),
        Err(PolicyError::BadDomain("Example.com".into())),
        "not as the editor writes it"
    );
    assert!(matches!(
        read("net_allow = [\"https://example.com\"]\n"),
        Err(PolicyError::BadDomain(_))
    ));
    let list = |n: usize| {
        let names: Vec<String> = (0..n).map(|i| format!("\"s{i}.example.com\"")).collect();
        format!("net_allow = [{}]\n", names.join(", "))
    };
    assert!(read(&list(MAX_NET_ALLOW)).is_ok());
    assert_eq!(
        read(&list(MAX_NET_ALLOW + 1)),
        Err(PolicyError::TooManyDomains(MAX_NET_ALLOW + 1))
    );
}
