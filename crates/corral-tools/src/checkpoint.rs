//! The checkpoint desk: where calls the policy asked about wait for the user (docs/01 §5).
//!
//! - **Single use.** An answer takes the checkpoint off the desk, so a replayed or stale answer
//!   finds nothing (TP-IPC-02). IDs come from the runner's [`crate::IdSource`]; the app's are
//!   random, so a checkpoint can't be answered without having been shown.
//! - **The gesture is checked by the policy** ([`PendingApproval::approve`]). One that isn't enough
//!   for the risk is refused and the checkpoint stays open (TP-CHK-04).
//! - **Stop or a timeout withdraws a checkpoint**; an answer after that does nothing.
//! - **An edit is an approval of the edited call** ([`PendingApproval::approve_edited`]): the same
//!   gesture, for that call only, its destination read again by the tool. Edited arguments are
//!   capped at [`MAX_EDIT_BYTES`].

use corral_core::domain::Risk;
use corral_core::ids::CheckpointId;
use corral_policy::{AllowToken, Approval, EditRefusal, PendingApproval, Scope};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::oneshot;

/// The longest denial note the agent and the trace get, in characters.
pub const MAX_NOTE_CHARS: usize = 1_000;

/// The largest edited arguments the desk takes, in bytes of JSON.
pub const MAX_EDIT_BYTES: usize = 16_384;

/// Reads a call's destination from its arguments: the tool's own reading (`Tool::destination`).
pub(crate) type Locate = Box<dyn Fn(&Value) -> Option<String> + Send + Sync>;

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
    /// Run it with these arguments instead of the agent's, approved with `how`, once.
    Edit {
        /// The gesture used.
        how: Approval,
        /// The edited arguments.
        args: Value,
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
    /// This checkpoint can't be edited (a spend limit); it is still open.
    #[error("This can't be edited. Approve or deny it.")]
    NotEditable,
    /// The edited arguments are over [`MAX_EDIT_BYTES`]; the checkpoint is still open.
    #[error("The edited arguments are too long.")]
    EditTooLarge,
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
    /// Approved with edited arguments: run the edited call this token allows.
    Edited {
        /// The token, carrying the edited call.
        token: AllowToken,
    },
    /// Denied.
    Denied {
        /// The capped note.
        note: Option<String>,
    },
}

struct Waiting {
    pending: PendingApproval,
    reply: oneshot::Sender<Outcome>,
    locate: Locate,
}

impl std::fmt::Debug for Waiting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waiting")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
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
        let Waiting {
            pending,
            reply,
            locate,
        } = open.remove(id).ok_or(DeskError::Unknown)?;
        // Puts a refused checkpoint back, still open.
        let mut keep = |pending, reply, locate, error| {
            open.insert(
                id.clone(),
                Waiting {
                    pending,
                    reply,
                    locate,
                },
            );
            Err(error)
        };
        let outcome = match answer {
            Answer::Approve { how, scope } => match pending.approve(how, scope) {
                Ok(token) => Outcome::Approved { token, scope },
                Err(refused) => {
                    let risk = refused.pending.risk();
                    return keep(
                        refused.pending,
                        reply,
                        locate,
                        DeskError::GestureNotEnough(risk),
                    );
                }
            },
            Answer::Edit { args, .. } if args.to_string().len() > MAX_EDIT_BYTES => {
                return keep(pending, reply, locate, DeskError::EditTooLarge);
            }
            Answer::Edit { how, args } => {
                let destination = locate(&args);
                match pending.approve_edited(how, args, destination) {
                    Ok(token) => Outcome::Edited { token },
                    Err(refused) => {
                        let error = match refused.why {
                            EditRefusal::Gesture => {
                                DeskError::GestureNotEnough(refused.pending.risk())
                            }
                            EditRefusal::NotEditable => DeskError::NotEditable,
                        };
                        return keep(refused.pending, reply, locate, error);
                    }
                }
            }
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

    /// Puts a checkpoint on the desk; the answer arrives on the returned channel. `locate` reads
    /// the destination of edited arguments.
    pub(crate) fn open(
        &self,
        id: CheckpointId,
        pending: PendingApproval,
        locate: Locate,
    ) -> Result<oneshot::Receiver<Outcome>, DeskError> {
        let mut open = self.lock();
        if open.contains_key(&id) {
            return Err(DeskError::Duplicate);
        }
        let (reply, answer) = oneshot::channel();
        open.insert(
            id,
            Waiting {
                pending,
                reply,
                locate,
            },
        );
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

    #[test]
    fn a_waiting_checkpoint_debugs_its_call_and_hides_the_locator() {
        use corral_policy::{Action, Gate, Policy, Profile, ToolCall, Verdict};
        let gate = Gate::new(Policy::new(Profile::default()));
        let Verdict::Ask(pending) = gate.check(ToolCall {
            action: Action::new(corral_core::domain::ActionClass::CommsSend, "email.send")
                .to("a@example.test"),
            args: serde_json::json!({}),
        }) else {
            panic!("sending asks");
        };
        let desk = CheckpointDesk::default();
        let _answer = desk
            .open(CheckpointId::from("c1"), pending, Box::new(|_| None))
            .expect("opened");
        let debug = format!("{desk:?}");
        assert!(debug.contains("Waiting { pending:"), "{debug}");
        assert!(debug.contains("email.send"), "{debug}");
        assert!(debug.contains("repeated: None }, .. }"), "{debug}");
    }
}
