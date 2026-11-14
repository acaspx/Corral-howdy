//! One run's tool calls, from request to result (docs/01 §5, TP-ADP-02). Every call is logged
//! and decided before anything runs:
//!
//! `tool.requested` → `policy.decided` → (Ask: `checkpoint.opened` … `checkpoint.resolved`) →
//! `tool.started` → `tool.result`.
//!
//! The class comes from the gateway's registry, never from the model: a tool nobody registered is
//! `unknown`, which the policy always denies. A denied call never reaches `tool.started`
//! (EV-SAFE-03); the agent is told why and carries on.

use crate::ToolGateway;
use crate::checkpoint::{CheckpointDesk, Outcome};
use corral_core::domain::ActionClass;
use corral_core::event::{
    CancelledBy, CheckpointOpened, CheckpointResolved, EventBody, PolicyDecided, PolicyDecision,
    Resolution, RunCancelled, ToolRequested, ToolResult, ToolStarted,
};
use corral_core::ids::{CheckpointId, ToolCallId};
use corral_core::supervisor::{CancelToken, EventSink};
use corral_policy::{
    Action, AllowToken, Gate, OnTimeout, PendingApproval, Policy, Scope, ToolCall, Verdict, title,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The longest checkpoint detail, in characters. The detail crosses into the UI and Swift, so it's
/// bounded here (AGENTS.md: data that crosses into Swift is untrusted).
pub const MAX_DETAIL_CHARS: usize = 2_000;

/// Makes tool-call and checkpoint IDs. The app's are random; tests count.
pub type IdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// What the agent is told about one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAnswer {
    /// It ran.
    Ran {
        /// The output, capped for the model.
        output: String,
        /// Whether the output was cut.
        truncated: bool,
    },
    /// It ran and failed, or the gateway refused it.
    Failed {
        /// Why, in plain words.
        message: String,
    },
    /// It didn't run.
    Denied {
        /// Why, in plain words.
        reason: String,
        /// The user's note, if they wrote one.
        note: Option<String>,
    },
    /// The run has ended (Stop, Recall-all, or an expired checkpoint under `cancel_run`). The
    /// agent stops here.
    Stopped,
}

/// Takes one run's tool calls through its gate, the desk and the gateway.
pub struct ToolRunner {
    gate: Gate,
    gateway: Arc<ToolGateway>,
    desk: Arc<CheckpointDesk>,
    ids: IdSource,
}

/// How a wait at a checkpoint ended.
enum Waited {
    Answered(Outcome),
    Expired,
    Stopped,
}

impl ToolRunner {
    /// A runner for a new run: a fresh gate, so no grants or taint carry over (TP-POL-04).
    pub fn new(
        policy: Policy,
        gateway: Arc<ToolGateway>,
        desk: Arc<CheckpointDesk>,
        ids: IdSource,
    ) -> Self {
        Self {
            gate: Gate::new(policy),
            gateway,
            desk,
            ids,
        }
    }

    /// Requests, decides and (if allowed) runs one call to `tool`.
    pub async fn call(
        &self,
        tool: &str,
        args: Value,
        sink: &EventSink,
        cancel: &CancelToken,
    ) -> ToolAnswer {
        let id = ToolCallId::from((self.ids)());
        let class = self.gateway.class_of(tool).unwrap_or(ActionClass::Unknown);
        let requested = EventBody::ToolRequested(ToolRequested {
            tool_call_id: id.clone(),
            tool: tool.to_owned(),
            action_class: class,
            args: args.clone(),
        });
        if sink.emit(requested).is_err() {
            return ToolAnswer::Stopped;
        }
        let mut action = Action::new(class, tool);
        if let Some(destination) = self.gateway.destination_of(tool, &args) {
            action = action.to(destination);
        }
        let verdict = self.gate.check(ToolCall { action, args });
        let decision = match &verdict {
            Verdict::Allow(_) => PolicyDecision::Allow,
            Verdict::Ask(pending) => PolicyDecision::Ask {
                risk: pending.risk(),
                reason: pending.reason().to_owned(),
            },
            Verdict::Deny { reason } => PolicyDecision::Deny {
                reason: reason.clone(),
            },
        };
        let decided = EventBody::PolicyDecided(PolicyDecided {
            tool_call_id: id.clone(),
            decision,
        });
        if sink.emit(decided).is_err() {
            return ToolAnswer::Stopped;
        }
        match verdict {
            Verdict::Allow(token) => self.run(id, token, sink).await,
            Verdict::Ask(pending) => self.ask(id, pending, sink, cancel).await,
            Verdict::Deny { reason } => ToolAnswer::Denied { reason, note: None },
        }
    }

    /// Waits at a checkpoint, then runs or refuses the call.
    async fn ask(
        &self,
        id: ToolCallId,
        pending: PendingApproval,
        sink: &EventSink,
        cancel: &CancelToken,
    ) -> ToolAnswer {
        let checkpoint_id = CheckpointId::from((self.ids)());
        let class = pending.call().action.class;
        let opened = EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: checkpoint_id.clone(),
            tool_call_id: id.clone(),
            action_class: class,
            risk: pending.risk(),
            title: title(class).to_owned(),
            detail: detail(pending.call()),
        });
        // On the desk before it's shown, so it can be answered as soon as it is.
        let Ok(mut reply) = self.desk.open(checkpoint_id.clone(), pending) else {
            return ToolAnswer::Denied {
                reason: "Corral couldn't open a checkpoint for this, so it didn't run.".into(),
                note: None,
            };
        };
        if sink.emit(opened).is_err() {
            self.desk.withdraw(&checkpoint_id);
            return ToolAnswer::Stopped;
        }
        let profile = self.gate.profile();
        let waited = tokio::select! {
            // Stop first: a Stop and an answer at the same moment never runs the call.
            biased;
            () = cancel.cancelled() => Waited::Stopped,
            // A closed channel means nobody can answer any more: treat it as no answer.
            answer = &mut reply => answer.map_or(Waited::Expired, Waited::Answered),
            () = tokio::time::sleep(profile.checkpoint_timeout) => Waited::Expired,
        };
        let waited = match waited {
            Waited::Answered(outcome) => Waited::Answered(outcome),
            // An answer that took the checkpoint just now is in the channel; the run still stops.
            Waited::Stopped => {
                self.desk.withdraw(&checkpoint_id);
                return ToolAnswer::Stopped;
            }
            Waited::Expired if self.desk.withdraw(&checkpoint_id) => Waited::Expired,
            Waited::Expired => reply.try_recv().map_or(Waited::Expired, Waited::Answered),
        };
        let (resolution, then) = match waited {
            Waited::Answered(Outcome::Approved { token, scope }) => {
                let resolution = match scope {
                    Scope::Once => Resolution::Approved,
                    Scope::ForRun => Resolution::AllowedForRun,
                };
                (resolution, Then::Run(token))
            }
            Waited::Answered(Outcome::Denied { note }) => (
                Resolution::Denied { note: note.clone() },
                Then::Answer(ToolAnswer::Denied {
                    reason: "You denied this, so it didn't run.".into(),
                    note,
                }),
            ),
            Waited::Expired => (
                Resolution::Expired,
                match profile.on_timeout {
                    OnTimeout::Deny => Then::Answer(ToolAnswer::Denied {
                        reason: "Nobody approved this in time, so it didn't run.".into(),
                        note: None,
                    }),
                    OnTimeout::CancelRun => Then::CancelRun,
                },
            ),
            Waited::Stopped => return ToolAnswer::Stopped,
        };
        let resolved = EventBody::CheckpointResolved(CheckpointResolved {
            checkpoint_id,
            resolution,
        });
        if sink.emit(resolved).is_err() {
            return ToolAnswer::Stopped;
        }
        match then {
            Then::Run(_) if cancel.is_cancelled() => ToolAnswer::Stopped,
            Then::Run(token) => self.run(id, token, sink).await,
            Then::Answer(answer) => answer,
            Then::CancelRun => {
                // If the log refuses this the run has ended anyway; either way the agent stops.
                let cancelled = sink.emit(EventBody::RunCancelled(RunCancelled {
                    by: CancelledBy::CheckpointExpired,
                }));
                drop(cancelled);
                ToolAnswer::Stopped
            }
        }
    }

    /// Runs an allowed call and logs its result.
    async fn run(&self, id: ToolCallId, token: AllowToken, sink: &EventSink) -> ToolAnswer {
        let started = EventBody::ToolStarted(ToolStarted {
            tool_call_id: id.clone(),
        });
        if sink.emit(started).is_err() {
            return ToolAnswer::Stopped;
        }
        let (ok, output, answer) = match self.gateway.run(token).await {
            Ok(out) => (
                true,
                Value::String(out.full),
                ToolAnswer::Ran {
                    output: out.for_model,
                    truncated: out.truncated,
                },
            ),
            Err(error) => {
                let message = error.to_string();
                (
                    false,
                    json!({ "error": message }),
                    ToolAnswer::Failed { message },
                )
            }
        };
        let result = EventBody::ToolResult(ToolResult {
            tool_call_id: id,
            ok,
            output,
        });
        if sink.emit(result).is_err() {
            return ToolAnswer::Stopped;
        }
        answer
    }
}

/// What happens after a checkpoint is resolved.
enum Then {
    Run(AllowToken),
    Answer(ToolAnswer),
    CancelRun,
}

/// The checkpoint's exact detail: the tool, where it goes, and its arguments verbatim, capped at
/// [`MAX_DETAIL_CHARS`] with a marker.
fn detail(call: &ToolCall) -> String {
    let mut text = match &call.action.destination {
        Some(destination) => format!("{} to {destination}\n", call.action.tool),
        None => format!("{}\n", call.action.tool),
    };
    text.push_str(&call.args.to_string());
    if text.chars().count() <= MAX_DETAIL_CHARS {
        return text;
    }
    let marker = "\n[Cut: the full arguments are in the trace.]";
    let keep = MAX_DETAIL_CHARS - marker.chars().count();
    let mut cut: String = text.chars().take(keep).collect();
    cut.push_str(marker);
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(destination: Option<&str>, args: Value) -> ToolCall {
        let mut action = Action::new(ActionClass::CommsSend, "email.send");
        if let Some(d) = destination {
            action = action.to(d);
        }
        ToolCall { action, args }
    }

    #[test]
    fn the_detail_names_the_tool_the_destination_and_the_arguments() {
        assert_eq!(
            detail(&call(
                Some("a@example.test"),
                json!({"to": "a@example.test"})
            )),
            "email.send to a@example.test\n{\"to\":\"a@example.test\"}"
        );
        assert_eq!(detail(&call(None, json!({}))), "email.send\n{}");
    }

    #[test]
    fn a_long_detail_is_cut_to_the_cap_with_a_marker() {
        let long = detail(&call(None, json!({"body": "é".repeat(5_000)})));
        assert_eq!(long.chars().count(), MAX_DETAIL_CHARS);
        assert!(long.ends_with("[Cut: the full arguments are in the trace.]"));
        let exact = "x".repeat(MAX_DETAIL_CHARS - "email.send\n\"\"".len());
        let fits = detail(&call(None, Value::String(exact)));
        assert_eq!(fits.chars().count(), MAX_DETAIL_CHARS);
        assert!(!fits.contains("[Cut"));
    }
}
