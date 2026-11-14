//! The checkpoint desk: where calls the policy asked about wait for the user (docs/01 §5).
//!
//! - **Single use.** An answer takes the checkpoint off the desk, so a replayed or stale answer
//!   finds nothing (TP-IPC-02). IDs come from the runner's [`crate::IdSource`]; the app's are
//!   random, so a checkpoint can't be answered without having been shown.
//! - **The gesture is checked by the policy** ([`PendingApproval::approve`]). One that isn't enough
//!   for the risk is refused and the checkpoint stays open (TP-CHK-04).
//! - **Stop or a timeout withdraws a checkpoint**; an answer after that does nothing.

use corral_core::domain::Risk;
use corral_core::ids::CheckpointId;
use corral_policy::{AllowToken, Approval, PendingApproval, Scope};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::oneshot;

/// The longest denial note the agent and the trace get, in characters.
pub const MAX_NOTE_CHARS: usize = 1_000;

/// The user's answer to one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Run it, approved with `how`.
    Approve {
        /// The gesture used.
        how: Approval,
        /// This call only, or the rest of the run.
        scope: Scope,
    },
    /// Don't run it, with an optional note for the agent.
    Deny {
        /// What to tell the agent.
        note: Option<String>,
    },
}

/// Why an answer wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeskError {
    /// Nothing is waiting under that ID: it was answered, it expired, or its run stopped.
    #[error("that checkpoint isn't waiting any more")]
    Unknown,
    /// The gesture isn't enough for the action's risk; the checkpoint is still open.
    #[error("{}", gesture_needed(*.0))]
    GestureNotEnough(Risk),
    /// A checkpoint with that ID is already waiting.
    #[error("a checkpoint with that ID is already waiting")]
    Duplicate,
}

fn gesture_needed(risk: Risk) -> &'static str {
    match risk {
        Risk::Critical => "This needs Touch ID.",
        Risk::High => "This can't be approved by voice.",
        Risk::Normal => "This approval wasn't accepted.",
    }
}

/// What the runner hears back.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Approved: run the call this token allows.
    Approved {
        /// The token.
        token: AllowToken,
        /// How long the approval lasts.
        scope: Scope,
    },
    /// Denied.
    Denied {
        /// The capped note.
        note: Option<String>,
    },
}

#[derive(Debug)]
struct Waiting {
    pending: PendingApproval,
    reply: oneshot::Sender<Outcome>,
}

/// Every checkpoint waiting on the user, across runs. Shared by the runners and the app.
#[derive(Debug, Default)]
pub struct CheckpointDesk {
    open: Mutex<HashMap<CheckpointId, Waiting>>,
}

impl CheckpointDesk {
    /// Answers checkpoint `id`. Each checkpoint takes one answer that's accepted.
    pub fn answer(&self, id: &CheckpointId, answer: Answer) -> Result<(), DeskError> {
        let mut open = self.lock();
        let Waiting { pending, reply } = open.remove(id).ok_or(DeskError::Unknown)?;
        let outcome = match answer {
            Answer::Approve { how, scope } => match pending.approve(how, scope) {
                Ok(token) => Outcome::Approved { token, scope },
                Err(refused) => {
                    let risk = refused.pending.risk();
                    open.insert(
                        id.clone(),
                        Waiting {
                            pending: refused.pending,
                            reply,
                        },
                    );
                    return Err(DeskError::GestureNotEnough(risk));
                }
            },
            Answer::Deny { note } => {
                // The call comes back only so it can be dropped here: nothing runs.
                drop(pending.deny());
                Outcome::Denied {
                    note: note.and_then(capped_note),
                }
            }
        };
        // Sent under the lock, so a runner that fails to withdraw a checkpoint finds the answer.
        // A failed send means the run stopped between withdraw and now: nothing will run.
        reply.send(outcome).map_err(|_| DeskError::Unknown)
    }

    /// Whether no checkpoint is waiting.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// The checkpoints waiting, in no particular order.
    pub fn waiting(&self) -> Vec<CheckpointId> {
        self.lock().keys().cloned().collect()
    }

    /// Puts a checkpoint on the desk; the answer arrives on the returned channel.
    pub(crate) fn open(
        &self,
        id: CheckpointId,
        pending: PendingApproval,
    ) -> Result<oneshot::Receiver<Outcome>, DeskError> {
        let mut open = self.lock();
        if open.contains_key(&id) {
            return Err(DeskError::Duplicate);
        }
        let (reply, answer) = oneshot::channel();
        open.insert(id, Waiting { pending, reply });
        Ok(answer)
    }

    /// Takes a checkpoint off the desk without an answer. `false` if an answer already took it.
    pub(crate) fn withdraw(&self, id: &CheckpointId) -> bool {
        self.lock().remove(id).is_some()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<CheckpointId, Waiting>> {
        // A panic while holding the lock leaves plain data behind; it's still sound to use.
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A note trimmed and capped at [`MAX_NOTE_CHARS`]; an empty one is no note.
fn capped_note(note: String) -> Option<String> {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_NOTE_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_trimmed_capped_and_empty_ones_dropped() {
        assert_eq!(capped_note("  ".into()), None);
        assert_eq!(capped_note(" no ".into()).as_deref(), Some("no"));
        assert_eq!(
            capped_note("é".repeat(MAX_NOTE_CHARS + 1)).map(|n| n.chars().count()),
            Some(MAX_NOTE_CHARS)
        );
        assert_eq!(
            capped_note("a".repeat(MAX_NOTE_CHARS)).map(|n| n.len()),
            Some(MAX_NOTE_CHARS)
        );
    }

    #[test]
    fn the_gesture_message_names_what_is_needed() {
        assert_eq!(
            DeskError::GestureNotEnough(Risk::Critical).to_string(),
            "This needs Touch ID."
        );
        assert_eq!(
            DeskError::GestureNotEnough(Risk::High).to_string(),
            "This can't be approved by voice."
        );
        assert_eq!(
            DeskError::GestureNotEnough(Risk::Normal).to_string(),
            "This approval wasn't accepted."
        );
    }
}
