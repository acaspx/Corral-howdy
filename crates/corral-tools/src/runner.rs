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
use corral_core::budget::usd;
use corral_core::domain::ActionClass;
use corral_core::event::{
    CancelledBy, CheckpointOpened, CheckpointResolved, EventBody, FailureReason, PolicyDecided,
    PolicyDecision, Resolution, RunCancelled, RunFailed, ToolRequested, ToolResult, ToolStarted,
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
    /// The desk refused the checkpoint (an ID already waiting).
    Unopened,
}

/// What the agent does at its soft limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAnswer {
    /// Approved (or allowed by the profile): requests go on, up to the hard cap.
    GoOn,
    /// The run has ended.
    Stop,
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
        if !decided(sink, &id, &verdict) {
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
        let class = pending.call().action.class;
        // The loop detector asked: say so first, so the card shows why.
        let text = match pending.repeated() {
            Some(n) => capped(format!(
                "Same call {n} times in this run.\n{}",
                full_detail(pending.call())
            )),
            None => detail(pending.call()),
        };
        let (checkpoint_id, waited) = self.wait(&id, class, text, pending, sink, cancel).await;
        let profile = self.gate.profile();
        let (resolution, then) = match waited {
            Waited::Answered(Outcome::Approved { token, scope }) => {
                (approved(scope), Then::Run(token))
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
            Waited::Unopened => {
                return ToolAnswer::Denied {
                    reason: "Corral couldn't open a checkpoint for this, so it didn't run.".into(),
                    note: None,
                };
            }
            Waited::Stopped => return ToolAnswer::Stopped,
        };
        if !resolve(sink, checkpoint_id, resolution) {
            return ToolAnswer::Stopped;
        }
        match then {
            Then::Run(_) if cancel.is_cancelled() => ToolAnswer::Stopped,
            Then::Run(token) => self.run(id, token, sink).await,
            Then::Answer(answer) => answer,
            Then::CancelRun => {
                end(
                    sink,
                    EventBody::RunCancelled(RunCancelled {
                        by: CancelledBy::CheckpointExpired,
                    }),
                );
                ToolAnswer::Stopped
            }
        }
    }

    /// The run reached its soft limit (TP-BUD-02): asks whether to go on, as a
    /// `budget.threshold` checkpoint the policy decides on. Going on lifts the limit to the hard
    /// cap. Deny, Stop or no answer ends the run; an unanswered soft limit never means spend
    /// more, whatever the profile's `on_timeout` says.
    pub async fn past_soft_limit(&self, sink: &EventSink, cancel: &CancelToken) -> BudgetAnswer {
        let Ok((committed, soft, cap)) = sink.budget_status() else {
            return BudgetAnswer::Stop;
        };
        let soft = soft.unwrap_or(cap);
        let id = ToolCallId::from((self.ids)());
        let call = ToolCall {
            action: Action::new(ActionClass::BudgetThreshold, "budget.soft_limit"),
            args: json!({
                "spent_micro_usd": committed,
                "soft_limit_micro_usd": soft,
                "cap_micro_usd": cap,
            }),
        };
        let verdict = self.gate.check(call);
        if !decided(sink, &id, &verdict) {
            return BudgetAnswer::Stop;
        }
        let pending = match verdict {
            Verdict::Allow(_) => return go_on(sink),
            Verdict::Deny { .. } => {
                end(
                    sink,
                    EventBody::RunFailed(RunFailed {
                        reason: FailureReason::BudgetHardCap,
                        message: format!(
                            "This run reached its {} soft limit, and its profile doesn't allow going past it.",
                            usd(soft)
                        ),
                    }),
                );
                return BudgetAnswer::Stop;
            }
            Verdict::Ask(pending) => pending,
        };
        let text = format!(
            "This run has used {}; its soft limit is {}.\nApprove to let it go on, up to its {} cap.",
            usd(committed),
            usd(soft),
            usd(cap)
        );
        let class = ActionClass::BudgetThreshold;
        let (checkpoint_id, waited) = self.wait(&id, class, text, pending, sink, cancel).await;
        let (resolution, then) = match waited {
            Waited::Answered(Outcome::Approved { scope, .. }) => (approved(scope), None),
            Waited::Answered(Outcome::Denied { note }) => {
                (Resolution::Denied { note }, Some(CancelledBy::User))
            }
            Waited::Expired => (Resolution::Expired, Some(CancelledBy::CheckpointExpired)),
            Waited::Unopened => {
                end(
                    sink,
                    EventBody::RunFailed(RunFailed {
                        reason: FailureReason::BudgetHardCap,
                        message: format!(
                            "This run reached its {} soft limit, and Corral couldn't ask you about going on.",
                            usd(soft)
                        ),
                    }),
                );
                return BudgetAnswer::Stop;
            }
            Waited::Stopped => return BudgetAnswer::Stop,
        };
        if !resolve(sink, checkpoint_id, resolution) {
            return BudgetAnswer::Stop;
        }
        match then {
            None if cancel.is_cancelled() => BudgetAnswer::Stop,
            None => go_on(sink),
            Some(by) => {
                end(sink, EventBody::RunCancelled(RunCancelled { by }));
                BudgetAnswer::Stop
            }
        }
    }

    /// Opens a checkpoint for `id` and waits for the user, a timeout or Stop. Returns the
    /// checkpoint's ID and how the wait ended; the caller logs `checkpoint.resolved`.
    async fn wait(
        &self,
        id: &ToolCallId,
        class: ActionClass,
        detail: String,
        pending: PendingApproval,
        sink: &EventSink,
        cancel: &CancelToken,
    ) -> (CheckpointId, Waited) {
        let checkpoint_id = CheckpointId::from((self.ids)());
        let opened = EventBody::CheckpointOpened(CheckpointOpened {
            checkpoint_id: checkpoint_id.clone(),
            tool_call_id: id.clone(),
            action_class: class,
            risk: pending.risk(),
            title: title(class).to_owned(),
            detail,
        });
        // On the desk before it's shown, so it can be answered as soon as it is.
        let Ok(mut reply) = self.desk.open(checkpoint_id.clone(), pending) else {
            return (checkpoint_id, Waited::Unopened);
        };
        if sink.emit(opened).is_err() {
            self.desk.withdraw(&checkpoint_id);
            return (checkpoint_id, Waited::Stopped);
        }
        let waited = tokio::select! {
            // Stop first: a Stop and an answer at the same moment never runs the call.
            biased;
            () = cancel.cancelled() => Waited::Stopped,
            // A closed channel means nobody can answer any more: treat it as no answer.
            answer = &mut reply => answer.map_or(Waited::Expired, Waited::Answered),
            () = tokio::time::sleep(self.gate.profile().checkpoint_timeout) => Waited::Expired,
        };
        let waited = match waited {
            Waited::Answered(outcome) => Waited::Answered(outcome),
            // An answer that took the checkpoint just now is in the channel; the run still stops.
            Waited::Stopped | Waited::Unopened => {
                self.desk.withdraw(&checkpoint_id);
                Waited::Stopped
            }
            Waited::Expired if self.desk.withdraw(&checkpoint_id) => Waited::Expired,
            Waited::Expired => reply.try_recv().map_or(Waited::Expired, Waited::Answered),
        };
        (checkpoint_id, waited)
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

/// Logs `policy.decided` for `id`. `false`: the run has ended.
fn decided(sink: &EventSink, id: &ToolCallId, verdict: &Verdict) -> bool {
    let decision = match verdict {
        Verdict::Allow(_) => PolicyDecision::Allow,
        Verdict::Ask(pending) => PolicyDecision::Ask {
            risk: pending.risk(),
            reason: pending.reason().to_owned(),
        },
        Verdict::Deny { reason } => PolicyDecision::Deny {
            reason: reason.clone(),
        },
    };
    sink.emit(EventBody::PolicyDecided(PolicyDecided {
        tool_call_id: id.clone(),
        decision,
    }))
    .is_ok()
}

/// Logs `checkpoint.resolved`. `false`: the run has ended.
fn resolve(sink: &EventSink, checkpoint_id: CheckpointId, resolution: Resolution) -> bool {
    sink.emit(EventBody::CheckpointResolved(CheckpointResolved {
        checkpoint_id,
        resolution,
    }))
    .is_ok()
}

fn approved(scope: Scope) -> Resolution {
    match scope {
        Scope::Once => Resolution::Approved,
        Scope::ForRun => Resolution::AllowedForRun,
    }
}

/// Lifts the soft limit to the cap.
fn go_on(sink: &EventSink) -> BudgetAnswer {
    if sink.raise_to_cap().is_ok() {
        BudgetAnswer::GoOn
    } else {
        BudgetAnswer::Stop
    }
}

/// Logs the event that ends the run. If the log refuses it, the run has ended anyway; either way
/// the agent stops.
fn end(sink: &EventSink, terminal: EventBody) {
    let ended = sink.emit(terminal);
    drop(ended);
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
    capped(full_detail(call))
}

/// The tool, where it goes, and its arguments verbatim.
fn full_detail(call: &ToolCall) -> String {
    let mut text = match &call.action.destination {
        Some(destination) => format!("{} to {destination}\n", call.action.tool),
        None => format!("{}\n", call.action.tool),
    };
    text.push_str(&call.args.to_string());
    text
}

/// `text` cut to [`MAX_DETAIL_CHARS`] with a marker.
fn capped(text: String) -> String {
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
