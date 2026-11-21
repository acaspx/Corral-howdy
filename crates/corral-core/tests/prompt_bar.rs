//! The prompt bar's rules (docs/07 §6.1): what it shows while you type, and who Return or Deploy
//! all sends to. Swift only shows the answer (TP-BAR-02, 03, 05).

use corral_core::controls::ControlError;
use corral_core::ids::{AgentId, RunId};
use corral_core::mention::Addressee;
use corral_core::prompt_bar::{BarAgent, BarStatus, How, Sent, send, view};
use std::cell::RefCell;

fn agent(id: &str, name: &str, shown: bool, status: BarStatus) -> BarAgent {
    BarAgent {
        agent: Addressee {
            id: AgentId::from(id),
            name: name.to_owned(),
            shown,
        },
        status,
    }
}

/// The corral's order: Grok, Claude and GPT shown (GPT working), Muse Spark hidden.
fn agents() -> Vec<BarAgent> {
    vec![
        agent("grok", "Grok", true, BarStatus::Ready),
        agent("claude", "Claude", true, BarStatus::Ready),
        agent("gpt", "GPT", true, BarStatus::Working),
        agent("muse", "Muse Spark", false, BarStatus::Ready),
    ]
}

fn ids(list: &[&str]) -> Vec<AgentId> {
    list.iter().map(|id| AgentId::from(*id)).collect()
}

/// Sends with a deploy that records each call, refusing the agents in `busy`.
fn sending(
    text: &str,
    selection: &[&str],
    how: How,
    busy: &[&str],
) -> (Result<Sent, String>, Vec<(String, String)>) {
    let calls = RefCell::new(Vec::new());
    let result = send(text, &ids(selection), &agents(), how, |agent, task| {
        calls
            .borrow_mut()
            .push((agent.as_str().to_owned(), task.to_owned()));
        if busy.contains(&agent.as_str()) {
            Err(ControlError::Busy(agent.clone()))
        } else {
            Ok(RunId::from(format!("run-{}", agent.as_str()).as_str()))
        }
    });
    (result, calls.into_inner())
}

fn calls(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, t)| ((*a).to_owned(), (*t).to_owned()))
        .collect()
}

#[test]
fn tp_bar_02_names_go_first_then_the_selection_then_no_one() {
    let (sent, made) = sending("@grok @muse plan it", &["claude"], How::Return, &[]);
    assert_eq!(sent.expect("sent").agents, ids(&["grok", "muse"]));
    assert_eq!(
        made,
        calls(&[("grok", "plan it"), ("muse", "plan it")]),
        "a hidden agent, named"
    );

    let (sent, made) = sending("  plan it ", &["claude", "grok"], How::Return, &[]);
    assert_eq!(sent.expect("sent").agents, ids(&["claude", "grok"]));
    assert_eq!(made, calls(&[("claude", "plan it"), ("grok", "plan it")]));

    let (sent, made) = sending("plan it", &[], How::Return, &[]);
    let err = sent.expect_err("no one");
    assert!(err.contains("Deploy all"), "{err}");
    assert!(made.is_empty(), "Return never deploys all");
}

#[test]
fn tp_bar_02_deploy_all_sends_to_the_shown_ready_agents_only_when_no_one_is_chosen() {
    let (sent, made) = sending("plan it", &[], How::All, &[]);
    assert_eq!(sent.expect("sent").agents, ids(&["grok", "claude"]));
    assert_eq!(
        made,
        calls(&[("grok", "plan it"), ("claude", "plan it")]),
        "not the working one, not the hidden one"
    );
    let (sent, _) = sending("@muse plan it", &[], How::All, &[]);
    assert_eq!(
        sent.expect("sent").agents,
        ids(&["muse"]),
        "names still win"
    );
    let (sent, _) = sending("plan it", &["claude"], How::All, &[]);
    assert_eq!(
        sent.expect("sent").agents,
        ids(&["claude"]),
        "and the selection"
    );
}

#[test]
fn tp_bar_02_the_selection_is_checked_and_deduped() {
    let (sent, made) = sending("go", &["claude", "stranger", "claude"], How::Return, &[]);
    assert_eq!(sent.expect("sent").agents, ids(&["claude"]));
    assert_eq!(made.len(), 1);
    let (sent, made) = sending("go", &["stranger"], How::Return, &[]);
    assert!(
        sent.is_err() && made.is_empty(),
        "an unknown selection is no selection"
    );
}

#[test]
fn tp_bar_03_busy_agents_are_skipped_and_named() {
    let (sent, made) = sending("@grok @claude @gpt go", &[], How::Return, &["gpt"]);
    let sent = sent.expect("some sent");
    assert_eq!(sent.agents, ids(&["grok", "claude"]));
    assert_eq!(
        sent.note.as_deref(),
        Some("Sent to Grok and Claude. GPT is busy.")
    );
    assert_eq!(made.len(), 3);

    let (sent, _) = sending("@grok @claude go", &[], How::Return, &[]);
    assert_eq!(
        sent.expect("sent").note,
        None,
        "nothing to say when all were sent"
    );

    let (sent, _) = sending("@grok @claude go", &[], How::Return, &["grok", "claude"]);
    assert_eq!(sent.expect_err("none sent"), "Grok and Claude are busy.");

    let (sent, _) = sending("go", &[], How::All, &["grok"]);
    assert_eq!(
        sent.expect("claude sent").note.as_deref(),
        Some("Sent to Claude. Grok is busy.")
    );
}

#[test]
fn tp_bar_03_another_refusal_is_named_with_the_cores_reason() {
    let result = send(
        "@grok @claude go",
        &[],
        &agents(),
        How::Return,
        |agent, _| {
            if agent.as_str() == "claude" {
                Err(ControlError::PromptTooLong { max_bytes: 8 })
            } else {
                Ok(RunId::from("run-1"))
            }
        },
    );
    let note = result.expect("grok sent").note.expect("a note");
    assert!(
        note.starts_with("Sent to Grok. Claude couldn't start: That task is too long"),
        "{note}"
    );
}

#[test]
fn tp_bar_05_a_bad_prompt_sends_nothing_and_says_why() {
    for (text, says) in [
        ("@nobody go", "No agent is called nobody."),
        ("@ go", "Write an agent's name after @."),
        ("@grok", "Write a task after the agent's name."),
        ("   ", "Write a task after the agent's name."),
    ] {
        let (sent, made) = sending(text, &["claude"], How::All, &[]);
        assert_eq!(sent.expect_err(text), says);
        assert!(made.is_empty(), "{text:?}");
    }
}

#[test]
fn tp_bar_02_the_bar_shows_who_it_goes_to_and_what_the_button_does() {
    let all = agents();
    let shown = |text: &str, selection: &[&str]| view(text, text, &ids(selection), &all);

    let empty = shown("", &[]);
    assert!(empty.chips.is_empty());
    assert_eq!(
        empty.placeholder,
        "Type @ and a name, or pick agents in the corral"
    );
    assert_eq!(
        (empty.button.label.as_str(), empty.button.all),
        ("Deploy all (2)", true)
    );
    assert!(empty.button.enabled);

    let one = shown("@grok ", &[]);
    assert_eq!(one.chips, ["Grok"]);
    assert_eq!(
        (one.button.label.as_str(), one.button.all),
        ("Deploy", false)
    );

    let three = shown("@grok @claude @Muse Spark do", &[]);
    assert_eq!(three.chips, ["Grok", "Claude", "Muse Spark"]);
    assert_eq!(three.button.label, "Deploy to 3");

    let picked = shown("", &["grok", "claude", "gpt"]);
    assert_eq!(picked.chips, ["3 selected"]);
    assert_eq!(picked.placeholder, "Give Grok, Claude and GPT a task");
    assert_eq!(picked.button.label, "Deploy to 3");
    assert_eq!(shown("", &["claude"]).placeholder, "Give Claude a task");
    assert_eq!(shown("", &["claude"]).chips, ["Claude"]);

    let busy_all = view(
        "",
        "",
        &[],
        &[agent("gpt", "GPT", true, BarStatus::NeedsYou)],
    );
    assert_eq!(busy_all.button.label, "Deploy all (0)");
    assert!(!busy_all.button.enabled);
}

#[test]
fn tp_bar_05_the_error_line_waits_until_a_name_is_finished() {
    let all = agents();
    let line = |text: &str| view(text, text, &[], &all).error;
    assert_eq!(line("@nob"), None, "still typing");
    assert_eq!(
        line("@nobody "),
        Some("No agent is called nobody.".to_owned())
    );
    assert_eq!(line("@grok @nob"), None);
    assert_eq!(
        line("@grok, @nobody: go"),
        Some("No agent is called nobody.".to_owned())
    );
    assert_eq!(line("@"), None);
    assert_eq!(
        line("@ "),
        Some("Write an agent's name after @.".to_owned())
    );
    assert_eq!(line("@grok "), None, "a missing task waits for Return");
    assert_eq!(view("@grok @nob", "@grok @nob", &[], &all).chips, ["Grok"]);
}

#[test]
fn tp_bar_04_suggestions_carry_each_agents_status() {
    let all = agents();
    let rows = view("@", "@", &[], &all).suggestions;
    let seen: Vec<(&str, bool, BarStatus)> = rows
        .iter()
        .map(|r| (r.name.as_str(), r.shown, r.status))
        .collect();
    assert_eq!(
        seen,
        [
            ("Grok", true, BarStatus::Ready),
            ("Claude", true, BarStatus::Ready),
            ("GPT", true, BarStatus::Working),
            ("Muse Spark", false, BarStatus::Ready)
        ]
    );
    assert_eq!(rows[1].completion, "@Claude ");
    let mid = view("@cl and more", "@cl", &[], &all).suggestions;
    assert_eq!(mid.len(), 1, "from the text before the cursor");
    assert!(
        view("@cl", "@x", &[], &all).suggestions.is_empty(),
        "a cursor text that isn't the start of the text is ignored"
    );
}

#[test]
fn tp_bar_03_a_reason_without_a_full_stop_gets_one() {
    use corral_core::supervisor::SupervisorError;
    let result = send(
        "@grok @claude go",
        &[],
        &agents(),
        How::Return,
        |agent, _| {
            if agent.as_str() == "claude" {
                Err(ControlError::Supervisor(SupervisorError::Poisoned))
            } else {
                Ok(RunId::from("run-1"))
            }
        },
    );
    let note = result.expect("grok sent").note.expect("a note");
    assert_eq!(
        note,
        "Sent to Grok. Claude couldn't start: the supervisor's state is poisoned."
    );
}
