//! The prompt bar's rules (docs/07 §6.1, TP-BAR-02..05): what it shows while you type, and who
//! Return or Deploy all sends to. Swift only shows the answer (AGENTS.md rule 3).
//!
//! - **Who it goes to:** the names at the start of the prompt ([`crate::mention`]), else the drops
//!   ⌘-clicked in the corral, else no one. Return never sends to more than was named or selected;
//!   only Deploy all does, to the agents the corral shows that are ready. Hidden agents only work
//!   when named.
//! - **Busy agents** are skipped: the others get the task, and a note names who didn't.
//! - **What it shows:** a chip per agent it goes to, a placeholder saying what Return does, the
//!   send button's label and count, an error line once a name is finished, and the suggestions
//!   with each agent's status.

use crate::controls::ControlError;
use crate::ids::{AgentId, RunId};
use crate::mention::{self, Addressee};
use serde::Serialize;

/// What Return says when it has no one to send to.
pub const NO_ONE: &str = "Name an agent with @, pick some in the corral, or use Deploy all.";

/// What the placeholder says when no one is chosen.
pub const PICK_SOMEONE: &str = "Type @ and a name, or pick agents in the corral";

/// Whether an agent can take a task now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BarStatus {
    /// It can take a task.
    Ready,
    /// It's working.
    Working,
    /// It's waiting on the user.
    NeedsYou,
}

/// An agent, as the prompt bar sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarAgent {
    /// Who it is and whether the corral shows it.
    pub agent: Addressee,
    /// Whether it can take a task now.
    pub status: BarStatus,
}

/// How the prompt is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// Return: to the agents named or selected.
    Return,
    /// Deploy all (or ⌘Return): the same, or with no one chosen, the shown agents that are ready.
    All,
}

/// What was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// The agents that got the task.
    pub agents: Vec<AgentId>,
    /// Who didn't, and why, when someone didn't. The bar stays open to show it.
    pub note: Option<String>,
}

/// The send button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendButton {
    /// "Deploy", "Deploy to 3" or "Deploy all (5)".
    pub label: String,
    /// Whether it sends now.
    pub enabled: bool,
    /// It's Deploy all.
    pub all: bool,
}

/// A suggested agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionRow {
    /// The agent.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Whether the corral shows it.
    pub shown: bool,
    /// Whether it can take a task now.
    pub status: BarStatus,
    /// The text with the name completed.
    pub completion: String,
}

/// What the prompt bar shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BarView {
    /// Who it goes to: a chip per agent named, the one selected, or "3 selected".
    pub chips: Vec<String>,
    /// What Return does, for an empty field.
    pub placeholder: String,
    /// The send button.
    pub button: SendButton,
    /// The error line.
    pub error: Option<String>,
    /// The agents the name being typed could be.
    pub suggestions: Vec<SuggestionRow>,
}

fn addressees(agents: &[BarAgent]) -> Vec<Addressee> {
    agents.iter().map(|a| a.agent.clone()).collect()
}

fn find<'a>(agents: &'a [BarAgent], id: &AgentId) -> Option<&'a BarAgent> {
    agents.iter().find(|a| &a.agent.id == id)
}

fn name_of(agents: &[BarAgent], id: &AgentId) -> String {
    find(agents, id).map_or_else(|| id.as_str().to_owned(), |a| a.agent.name.clone())
}

/// The selection, keeping only agents the bar knows, each once.
fn selected<'a>(selection: &[AgentId], agents: &'a [BarAgent]) -> Vec<&'a BarAgent> {
    let mut picked: Vec<&BarAgent> = Vec::new();
    for id in selection {
        if let Some(agent) = find(agents, id)
            && !picked.iter().any(|p| &p.agent.id == id)
        {
            picked.push(agent);
        }
    }
    picked
}

/// The shown agents that are ready: who Deploy all sends to.
fn everyone(agents: &[BarAgent]) -> Vec<&BarAgent> {
    agents
        .iter()
        .filter(|a| a.agent.shown && a.status == BarStatus::Ready)
        .collect()
}

/// "Grok", "Grok and Claude", "Grok, Claude and GPT".
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn deploy_label(count: usize) -> String {
    if count == 1 {
        "Deploy".to_owned()
    } else {
        format!("Deploy to {count}")
    }
}

/// What the bar shows for `text`, with the cursor after `before_cursor` (the start of `text`).
pub fn view(
    text: &str,
    before_cursor: &str,
    selection: &[AgentId],
    agents: &[BarAgent],
) -> BarView {
    let known = addressees(agents);
    let names = mention::names(text, &known);
    let error = names.problem.map(|e| e.to_string());
    let picked = selected(selection, agents);
    let picked_names: Vec<String> = picked.iter().map(|a| a.agent.name.clone()).collect();

    let (chips, count) = if !names.agents.is_empty() {
        let named: Vec<String> = names.agents.iter().map(|id| name_of(agents, id)).collect();
        let count = named.len();
        (named, count)
    } else if picked_names.len() > 1 {
        (
            vec![format!("{} selected", picked_names.len())],
            picked_names.len(),
        )
    } else {
        (picked_names.clone(), picked_names.len())
    };
    let button = if count > 0 {
        SendButton {
            label: deploy_label(count),
            enabled: error.is_none(),
            all: false,
        }
    } else {
        let ready = everyone(agents).len();
        SendButton {
            label: format!("Deploy all ({ready})"),
            enabled: ready > 0 && error.is_none(),
            all: true,
        }
    };
    let placeholder = if picked_names.is_empty() {
        PICK_SOMEONE.to_owned()
    } else {
        format!("Give {} a task", and_list(&picked_names))
    };
    let suggestions = if text.starts_with(before_cursor) {
        mention::suggest(before_cursor, &known)
            .into_iter()
            .filter_map(|s| {
                find(agents, &s.id).map(|a| SuggestionRow {
                    id: s.id.as_str().to_owned(),
                    name: s.name,
                    shown: s.shown,
                    status: a.status,
                    completion: s.completion,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    BarView {
        chips,
        placeholder,
        button,
        error,
        suggestions,
    }
}

/// Sends `text` the way `how` says, through `deploy` (the corral's controls). Nothing is sent
/// when the prompt is bad or there's no one to send to; otherwise each agent is tried, and the
/// note names who didn't get it.
pub fn send(
    text: &str,
    selection: &[AgentId],
    agents: &[BarAgent],
    how: How,
    mut deploy: impl FnMut(&AgentId, &str) -> Result<RunId, ControlError>,
) -> Result<Sent, String> {
    let prompt = mention::address(text, &addressees(agents)).map_err(|e| e.to_string())?;
    let targets: Vec<AgentId> = if !prompt.agents.is_empty() {
        prompt.agents
    } else {
        let picked = selected(selection, agents);
        if !picked.is_empty() {
            picked.iter().map(|a| a.agent.id.clone()).collect()
        } else {
            match how {
                How::Return => return Err(NO_ONE.to_owned()),
                How::All => everyone(agents)
                    .iter()
                    .map(|a| a.agent.id.clone())
                    .collect(),
            }
        }
    };
    if targets.is_empty() {
        return Err("No agent in the corral is ready for a task.".to_owned());
    }

    let mut sent = Vec::new();
    let mut busy = Vec::new();
    let mut refused = Vec::new();
    for agent in targets {
        match deploy(&agent, &prompt.task) {
            Ok(_) => sent.push(agent),
            Err(ControlError::Busy(_)) => busy.push(name_of(agents, &agent)),
            Err(e) => {
                let mut why = e.to_string();
                if !why.ends_with('.') {
                    why.push('.');
                }
                refused.push(format!("{} couldn't start: {why}", name_of(agents, &agent)));
            }
        }
    }
    let mut unsent = Vec::new();
    if !busy.is_empty() {
        let verb = if busy.len() == 1 { "is" } else { "are" };
        unsent.push(format!("{} {verb} busy.", and_list(&busy)));
    }
    unsent.extend(refused);
    if sent.is_empty() {
        return Err(unsent.join(" "));
    }
    let note = (!unsent.is_empty()).then(|| {
        let names: Vec<String> = sent.iter().map(|id| name_of(agents, id)).collect();
        format!("Sent to {}. {}", and_list(&names), unsent.join(" "))
    });
    Ok(Sent { agents: sent, note })
}
