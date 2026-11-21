//! `@` in the prompt bar (M4 item 2b, docs/07 §3): who a prompt is for, and what to suggest while
//! a name is typed (TP-PROMPT-01..05). Hidden agents aren't second-class: they're reachable here
//! the same way as the ones the corral shows.
//!
//! - **Addressing:** names at the start of the prompt, each after an `@`, send the rest to those
//!   agents. A name matches the agent's name in any case, with or without its spaces, or its ID; it
//!   ends at a space, `,`, `:` or `;`. A name later in the prompt is part of the task, and without
//!   one at the start the prompt isn't addressed (the bar uses what's selected).
//! - **Suggestions:** while a name at the start is being typed, the agents it could be, in the
//!   order given (the corral's, hidden ones last), with the text completed.

use crate::agent_text::AgentText;
use crate::ids::AgentId;

/// The most agents suggested at once.
pub const MAX_SUGGESTIONS: usize = 8;

/// The most of an unknown name an error repeats.
pub const MAX_NAME_CHARS: usize = 40;

/// An agent a prompt can be for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressee {
    /// The agent.
    pub id: AgentId,
    /// Its name, e.g. "Muse Spark".
    pub name: String,
    /// Whether the corral shows it.
    pub shown: bool,
}

/// Who a prompt is for, and the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The agents named, each once, in the order named. Empty: the prompt names no one.
    pub agents: Vec<AgentId>,
    /// The rest of the prompt, trimmed.
    pub task: String,
}

/// Why a prompt can't be sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MentionError {
    /// No agent has this name (cleaned and capped).
    #[error("No agent is called {0}.")]
    Unknown(String),
    /// An `@` with no name after it.
    #[error("Write an agent's name after @.")]
    NoName,
    /// Nothing after the names.
    #[error("Write a task after the agent's name.")]
    NoTask,
}

/// An agent the name being typed could be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The agent.
    pub id: AgentId,
    /// Its name.
    pub name: String,
    /// Whether the corral shows it.
    pub shown: bool,
    /// The text with the name completed, and a space after it.
    pub completion: String,
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | ':' | ';')
}

fn same(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

/// What an agent answers to: its name, its name without spaces, and its ID.
fn keys(agent: &Addressee) -> [String; 3] {
    [
        agent.name.clone(),
        agent.name.chars().filter(|c| !c.is_whitespace()).collect(),
        agent.id.as_str().to_owned(),
    ]
}

/// The bytes of `text` that `key` takes, if `text` starts with it (any case) and it ends there.
fn takes(text: &str, key: &str) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    let mut chars = text.char_indices();
    for k in key.chars() {
        let (_, t) = chars.next()?;
        if !same(t, k) {
            return None;
        }
    }
    match chars.next() {
        None => Some(text.len()),
        Some((i, c)) if is_separator(c) => Some(i),
        Some(_) => None,
    }
}

/// Whether `typed` could be the start of `key` (any case).
fn starts(typed: &str, key: &str) -> bool {
    let mut key = key.chars();
    typed
        .chars()
        .all(|t| key.next().is_some_and(|k| same(t, k)))
}

/// The agent named at the start of `text` (just after an `@`) and the bytes its name takes. The
/// longest name wins; of equal ones, the first agent.
fn named<'a>(text: &str, agents: &'a [Addressee]) -> Option<(&'a Addressee, usize)> {
    let mut best: Option<(&Addressee, usize)> = None;
    for agent in agents {
        let Some(len) = keys(agent).iter().filter_map(|k| takes(text, k)).max() else {
            continue;
        };
        if best.is_none_or(|(_, longest)| len > longest) {
            best = Some((agent, len));
        }
    }
    best
}

/// The names at the start of a prompt so far, for the prompt bar while it's typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// The agents named, each once, in the order named.
    pub agents: Vec<AgentId>,
    /// A name that's finished (a space or punctuation follows it) but isn't an agent's, or an `@`
    /// with no name. Nothing while a name is still being typed.
    pub problem: Option<MentionError>,
}

/// Where reading the names at the start of a prompt stopped.
enum Stop<'a> {
    /// At the task.
    Task(&'a str),
    /// At a name that isn't an agent's; `finished` when something follows it.
    Bad { error: MentionError, finished: bool },
}

fn parse<'a>(text: &'a str, agents: &[Addressee]) -> (Vec<AgentId>, Stop<'a>) {
    let mut rest = text.trim_start();
    let mut chosen: Vec<AgentId> = Vec::new();
    while let Some(after) = rest.strip_prefix('@') {
        let Some((agent, len)) = named(after, agents) else {
            let name = after.split(is_separator).next().unwrap_or_default();
            let finished = name.len() < after.len();
            let error = if name.is_empty() {
                MentionError::NoName
            } else {
                MentionError::Unknown(AgentText::new(name, MAX_NAME_CHARS).as_str().to_owned())
            };
            return (chosen, Stop::Bad { error, finished });
        };
        if !chosen.contains(&agent.id) {
            chosen.push(agent.id.clone());
        }
        rest = after[len..].trim_start_matches(is_separator);
    }
    (chosen, Stop::Task(rest))
}

/// Who `text` is for, and the task.
pub fn address(text: &str, agents: &[Addressee]) -> Result<Prompt, MentionError> {
    let (chosen, stop) = parse(text, agents);
    let task = match stop {
        Stop::Task(task) => task.trim(),
        Stop::Bad { error, .. } => return Err(error),
    };
    if task.is_empty() {
        return Err(MentionError::NoTask);
    }
    Ok(Prompt {
        agents: chosen,
        task: task.to_owned(),
    })
}

/// The names at the start of `text` so far, and any finished name that isn't an agent's.
pub fn names(text: &str, agents: &[Addressee]) -> Names {
    let (chosen, stop) = parse(text, agents);
    let problem = match stop {
        Stop::Bad {
            error,
            finished: true,
        } => Some(error),
        Stop::Bad {
            finished: false, ..
        }
        | Stop::Task(_) => None,
    };
    Names {
        agents: chosen,
        problem,
    }
}

/// The agents the name being typed at the end of `before_cursor` could be. Nothing unless a name
/// at the start of the prompt is being typed.
pub fn suggest(before_cursor: &str, agents: &[Addressee]) -> Vec<Suggestion> {
    let mut at = before_cursor.len() - before_cursor.trim_start().len();
    let mut chosen: Vec<&AgentId> = Vec::new();
    while let Some(after) = before_cursor[at..].strip_prefix('@') {
        let could_be: Vec<&Addressee> = agents
            .iter()
            .filter(|a| !chosen.contains(&&a.id))
            .filter(|a| keys(a).iter().any(|k| starts(after, k)))
            .take(MAX_SUGGESTIONS)
            .collect();
        if !could_be.is_empty() {
            let typed_so_far = &before_cursor[..at];
            return could_be
                .into_iter()
                .map(|a| Suggestion {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    shown: a.shown,
                    completion: format!("{typed_so_far}@{} ", a.name),
                })
                .collect();
        }
        // A finished name: on to the next one, if any.
        let Some((agent, len)) = named(after, agents) else {
            break;
        };
        chosen.push(&agent.id);
        let next = after[len..].trim_start_matches(is_separator);
        at = before_cursor.len() - next.len();
    }
    Vec::new()
}
