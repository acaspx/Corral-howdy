//! `@` in the prompt bar (M4 item 2b, docs/07 §3): who a prompt is for, and what to suggest while
//! typing a name. Hidden agents are reachable the same way as shown ones (TP-PROMPT-01..05).

use corral_core::ids::AgentId;
use corral_core::mention::{Addressee, MAX_SUGGESTIONS, MentionError, Prompt, address, suggest};

fn agent(id: &str, name: &str, shown: bool) -> Addressee {
    Addressee {
        id: AgentId::from(id),
        name: name.to_owned(),
        shown,
    }
}

/// The corral's order: shown ones first, then the hidden ones.
fn agents() -> Vec<Addressee> {
    vec![
        agent("grok", "Grok", true),
        agent("claude", "Claude", true),
        agent("muse", "Muse Spark", false),
        agent("gpt", "GPT", false),
    ]
}

fn to(text: &str) -> Prompt {
    address(text, &agents()).expect(text)
}

fn ids(prompt: &Prompt) -> Vec<&str> {
    prompt.agents.iter().map(AgentId::as_str).collect()
}

#[test]
fn tp_prompt_01_an_at_name_sends_the_rest_to_that_agent_hidden_or_not() {
    let p = to("@grok summarize this page");
    assert_eq!(
        (ids(&p), p.task.as_str()),
        (vec!["grok"], "summarize this page")
    );
    assert_eq!(ids(&to("@GROK hi")), ["grok"], "names match in any case");
    assert_eq!(ids(&to("@gpt hi")), ["gpt"], "a hidden agent is reachable");
    let p = to("@Muse Spark plan a trip");
    assert_eq!((ids(&p), p.task.as_str()), (vec!["muse"], "plan a trip"));
    assert_eq!(
        ids(&to("@musespark hi")),
        ["muse"],
        "spaces in a name are optional"
    );
    assert_eq!(ids(&to("@muse hi")), ["muse"], "the agent's ID works too");
    assert_eq!(to("@claude   two\nlines  ").task, "two\nlines");
}

#[test]
fn tp_prompt_02_several_names_send_the_task_to_each_once() {
    assert_eq!(ids(&to("@grok @claude compare these")), ["grok", "claude"]);
    assert_eq!(ids(&to("@grok, @claude: compare")), ["grok", "claude"]);
    assert_eq!(ids(&to("@grok @GROK @grok hi")), ["grok"]);
    let p = to("@grok ask @claude later");
    assert_eq!(
        (ids(&p), p.task.as_str()),
        (vec!["grok"], "ask @claude later")
    );
}

#[test]
fn tp_prompt_02_the_longest_name_wins_and_of_equal_names_the_first() {
    let agents = vec![
        agent("muse", "Muse", true),
        agent("muse-spark", "Muse Spark", false),
        agent("echo-1", "Echo", true),
        agent("echo-2", "Echo", true),
    ];
    let to = |text: &str| address(text, &agents).expect(text).agents;
    assert_eq!(to("@Muse Spark hi"), [AgentId::from("muse-spark")]);
    assert_eq!(to("@muse hi"), [AgentId::from("muse")]);
    assert_eq!(to("@echo hi"), [AgentId::from("echo-1")]);
}

#[test]
fn tp_prompt_03_without_a_leading_name_the_prompt_isnt_addressed() {
    let p = to("write to bob@grok.com");
    assert!(p.agents.is_empty());
    assert_eq!(p.task, "write to bob@grok.com");
}

#[test]
fn tp_prompt_04_an_unknown_name_or_a_missing_task_says_why() {
    let err = |text: &str| address(text, &agents()).expect_err(text);
    assert_eq!(
        err("@nobody do this"),
        MentionError::Unknown("nobody".into())
    );
    assert_eq!(
        err("@grokking it"),
        MentionError::Unknown("grokking".into()),
        "a name ends at a space or punctuation"
    );
    assert_eq!(
        err("@grok @nobody hi"),
        MentionError::Unknown("nobody".into())
    );
    assert_eq!(err("@ do this"), MentionError::NoName);
    assert_eq!(err("@grok"), MentionError::NoTask);
    assert_eq!(err("@grok @claude ,  "), MentionError::NoTask);
    assert_eq!(err("   "), MentionError::NoTask);
    assert_eq!(
        MentionError::Unknown("nobody".into()).to_string(),
        "No agent is called nobody."
    );
    let long = format!("@{}\u{202e}x hi", "n".repeat(500));
    let MentionError::Unknown(name) = err(&long) else {
        panic!("unknown");
    };
    assert!(
        name.chars().count() <= 40,
        "the name in the message is capped"
    );
    assert!(!name.contains('\u{202e}'), "and cleaned");
}

#[test]
fn tp_prompt_05_typing_a_name_suggests_agents_hidden_ones_marked() {
    let names = |text: &str| -> Vec<(String, bool)> {
        suggest(text, &agents())
            .into_iter()
            .map(|s| (s.name, s.shown))
            .collect()
    };
    assert_eq!(
        names("@"),
        [
            ("Grok".to_owned(), true),
            ("Claude".to_owned(), true),
            ("Muse Spark".to_owned(), false),
            ("GPT".to_owned(), false)
        ],
        "in the corral's order"
    );
    assert_eq!(names("@cl"), [("Claude".to_owned(), true)]);
    assert_eq!(names("@MU"), [("Muse Spark".to_owned(), false)]);
    assert_eq!(names("@Muse Sp"), [("Muse Spark".to_owned(), false)]);
    assert_eq!(
        names("@grok"),
        [("Grok".to_owned(), true)],
        "until the space"
    );
    assert_eq!(
        names("@grok @"),
        [
            ("Claude".to_owned(), true),
            ("Muse Spark".to_owned(), false),
            ("GPT".to_owned(), false)
        ],
        "not one already named"
    );
    for nothing in [
        "",
        "hi",
        "@grok ",
        "@grok do @c",
        "bob@g",
        "@zz",
        "@grok hi @",
    ] {
        assert!(names(nothing).is_empty(), "{nothing:?}");
    }
    let completion = |text: &str| suggest(text, &agents())[0].completion.clone();
    assert_eq!(completion("@cl"), "@Claude ");
    assert_eq!(completion("@grok, @mu"), "@grok, @Muse Spark ");

    let many: Vec<Addressee> = (0..20)
        .map(|i| agent(&format!("a{i}"), &format!("A{i}"), false))
        .collect();
    assert_eq!(suggest("@a", &many).len(), MAX_SUGGESTIONS);
}

proptest::proptest! {
    /// Anything typed is handled without a panic, and taking a suggestion then writing a task
    /// sends the task to that agent.
    #[test]
    fn tp_prompt_05_any_text_is_handled_and_a_suggestion_addresses_its_agent(
        text in "[@a-zA-Z ,:;\u{e9}\u{202e}\u{1f600}]{0,24}",
    ) {
        let agents = agents();
        let _ = address(&text, &agents);
        for s in suggest(&text, &agents) {
            let prompt = address(&format!("{}do it", s.completion), &agents)
                .expect("a completion addresses");
            proptest::prop_assert!(prompt.agents.contains(&s.id), "{:?}", s.completion);
            proptest::prop_assert_eq!(prompt.task.as_str(), "do it");
        }
    }
}
