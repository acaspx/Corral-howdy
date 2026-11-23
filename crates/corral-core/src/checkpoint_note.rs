//! The checkpoint notification (docs/01 "Notifications", docs/07 §8.2, TP-CHK-22, TP-CHK-23).
//!
//! Each checkpoint waiting on a card gets one macOS notification, carrying a fresh single-use
//! nonce. The core decides everything about it; Swift only posts what it's sent and reports the
//! click:
//! - **The buttons.** Only a normal-risk checkpoint offers Approve inline. High and critical offer
//!   Deny and Open, and the text says to approve on the card, where high needs a pinned card and
//!   the whole detail, and critical needs Touch ID (T18).
//! - **The nonce.** A click counts only with the nonce its notification carried, once, and only
//!   while its checkpoint still waits on that agent's card. Approve is refused for high and
//!   critical whatever Swift sends. The checkpoint desk and the policy then check the answer like
//!   any other.
//! - **Bounded.** At most [`MAX_NOTES`] notifications are tracked; the rest wait on their cards.
//!   If the user hasn't allowed Corral's notifications, none are posted for the rest of the
//!   session.

use crate::agent_text::AgentText;
use crate::domain::Risk;
use crate::ids::{AgentId, CheckpointId};
use crate::run_card::{CardCheckpoint, RunCards};
use std::collections::BTreeMap;

/// The most notifications tracked at once.
pub const MAX_NOTES: usize = 32;
/// The longest title, in characters (Swift keeps at most 120).
pub const TITLE_MAX_CHARS: usize = 100;
/// The longest body, in characters (Swift keeps at most 600).
pub const BODY_MAX_CHARS: usize = 400;
/// How much of the agent's name the title shows.
const NAME_MAX_CHARS: usize = 40;

/// A button on a checkpoint notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteOffer {
    /// Approve inline: normal risk only.
    Approve,
    /// Deny inline.
    Deny,
    /// Open the agent's card.
    Open,
}

/// The buttons a checkpoint of `risk` offers, in order.
pub fn offers(risk: Risk) -> Vec<NoteOffer> {
    match risk {
        Risk::Normal => vec![NoteOffer::Approve, NoteOffer::Deny, NoteOffer::Open],
        Risk::High | Risk::Critical => vec![NoteOffer::Deny, NoteOffer::Open],
    }
}

/// A notification to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The checkpoint it's for.
    pub checkpoint: CheckpointId,
    /// Its single-use nonce.
    pub nonce: String,
    /// The agent whose card it waits on.
    pub agent: AgentId,
    /// "Scout needs your approval": one line, cleaned and cut.
    pub title: String,
    /// The core's title and detail, on one line, cleaned and cut.
    pub body: String,
    /// The buttons, in order.
    pub offers: Vec<NoteOffer>,
}

/// What the user did with a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteClick {
    /// The inline Approve button.
    Approve,
    /// The inline Deny button.
    Deny,
    /// Open, or a click on the notification itself.
    Open,
    /// Dismissed without choosing.
    Dismiss,
}

/// What a click means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteAnswer {
    /// Answer `checkpoint` on `agent`'s card with a click, once.
    Approve {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint.
        checkpoint: CheckpointId,
    },
    /// Deny `checkpoint` on `agent`'s card.
    Deny {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint.
        checkpoint: CheckpointId,
    },
    /// Open `agent`'s card at `checkpoint`.
    Open {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint.
        checkpoint: CheckpointId,
    },
    /// Approve for a high or critical checkpoint, which the notification never offers. Nothing is
    /// answered; the checkpoint still waits on the card.
    Refused {
        /// The card's agent.
        agent: AgentId,
        /// The checkpoint.
        checkpoint: CheckpointId,
    },
    /// Dismissed: nothing to do.
    Dismissed,
    /// No notification with that nonce is out: a wrong, spent or old one, or its checkpoint no
    /// longer waits. Ignored.
    Stale,
}

/// Why a notification wasn't shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteTrouble {
    /// The user hasn't allowed Corral's notifications.
    NotAuthorized,
    /// macOS refused this one.
    Failed,
}

#[derive(Debug)]
struct Note {
    agent: AgentId,
    risk: Risk,
    /// `None` once a click has spent it.
    nonce: Option<String>,
}

/// The checkpoint notifications that are out. One per app.
#[derive(Debug, Default)]
pub struct NoteDesk {
    notes: BTreeMap<CheckpointId, Note>,
    muted: bool,
}

impl NoteDesk {
    /// Brings the desk in line with `cards`: forgets checkpoints that no longer wait, and returns
    /// a notice for each one waiting that has none yet, agents by ID and each agent's oldest
    /// first. `name` gives the name an agent's drop shows; `mint` gives each notice its nonce.
    pub fn observe(
        &mut self,
        cards: &RunCards,
        name: &dyn Fn(&AgentId) -> String,
        mint: &mut dyn FnMut() -> String,
    ) -> Vec<Notice> {
        self.notes
            .retain(|checkpoint, note| cards.waits_on(&note.agent, checkpoint));
        if self.muted {
            return Vec::new();
        }
        let mut notices = Vec::new();
        for agent in cards.agents() {
            for (checkpoint, _) in cards.waiting(&agent) {
                if self.notes.len() >= MAX_NOTES {
                    return notices;
                }
                if self.notes.contains_key(&checkpoint.id) {
                    continue;
                }
                let nonce = mint();
                self.notes.insert(
                    checkpoint.id.clone(),
                    Note {
                        agent: agent.clone(),
                        risk: checkpoint.risk,
                        nonce: Some(nonce.clone()),
                    },
                );
                notices.push(notice(&agent, &name(&agent), &checkpoint, nonce));
            }
        }
        notices
    }

    /// What a click on `checkpoint`'s notification means. It counts only with the nonce that
    /// notification carried, once.
    pub fn click(
        &mut self,
        checkpoint: &CheckpointId,
        nonce: &str,
        click: NoteClick,
    ) -> NoteAnswer {
        let Some(note) = self.notes.get_mut(checkpoint) else {
            return NoteAnswer::Stale;
        };
        if note.nonce.as_deref() != Some(nonce) {
            return NoteAnswer::Stale;
        }
        note.nonce = None;
        let agent = note.agent.clone();
        let checkpoint = checkpoint.clone();
        match click {
            NoteClick::Approve => match note.risk {
                Risk::Normal => NoteAnswer::Approve { agent, checkpoint },
                Risk::High | Risk::Critical => NoteAnswer::Refused { agent, checkpoint },
            },
            NoteClick::Deny => NoteAnswer::Deny { agent, checkpoint },
            NoteClick::Open => NoteAnswer::Open { agent, checkpoint },
            NoteClick::Dismiss => NoteAnswer::Dismissed,
        }
    }

    /// Swift couldn't show `checkpoint`'s notification. Its card still shows it. If the user
    /// hasn't allowed notifications, none are posted for the rest of the session.
    pub fn failed(&mut self, checkpoint: &CheckpointId, trouble: NoteTrouble) {
        if let Some(note) = self.notes.get_mut(checkpoint) {
            note.nonce = None;
        }
        match trouble {
            NoteTrouble::NotAuthorized => self.muted = true,
            NoteTrouble::Failed => {}
        }
    }

    /// Whether notifications are off for the session.
    pub fn muted(&self) -> bool {
        self.muted
    }
}

fn notice(agent: &AgentId, name: &str, checkpoint: &CardCheckpoint, nonce: String) -> Notice {
    let name = one_line(name, NAME_MAX_CHARS);
    let title = one_line(&format!("{name} needs your approval"), TITLE_MAX_CHARS);
    let how = match checkpoint.risk {
        Risk::Normal => "",
        Risk::High => " Approve it on its card.",
        Risk::Critical => " Approve it on its card with Touch ID.",
    };
    // The advice always fits: the detail is cut to make room for it.
    let room = BODY_MAX_CHARS.saturating_sub(how.chars().count());
    let what = one_line(
        &format!(
            "{}: {}",
            checkpoint.title.as_str(),
            checkpoint.detail.as_str()
        ),
        room,
    );
    Notice {
        checkpoint: checkpoint.id.clone(),
        nonce,
        agent: agent.clone(),
        title,
        body: format!("{what}{how}"),
        offers: offers(checkpoint.risk),
    }
}

/// `raw` cleaned, its line breaks made spaces, cut to `max` characters.
fn one_line(raw: &str, max: usize) -> String {
    let clean = AgentText::new(raw, usize::MAX);
    let flat = clean
        .as_str()
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    AgentText::new(&flat, max).as_str().to_owned()
}
