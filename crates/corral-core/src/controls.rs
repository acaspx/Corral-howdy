//! What a click on a drop does (SK-6, TP-SKEL-03..05). The surfaces only report the click; the
//! decision is made here (AGENTS.md rule 3).
//!
//! Each agent works on one session at a time (system design §3):
//! - **Deploy** from an agent in the corral starts a new session. From a done or failed agent
//!   it's a follow-up in the same session ("prompt again"). From a stopped agent, the stopped
//!   session is closed first and a new one starts. A working agent takes no second task.
//! - **Stop** cancels the working run.
//! - **Send back** closes the session once its run has ended; the drop is ready again.
//! - **Retry** reruns a failed run's prompt in the same session.
//!
//! Prompts are trimmed, must not be empty, and are at most [`MAX_PROMPT_BYTES`]. Run and session
//! IDs come from the ID source the caller gives, so tests are deterministic.

use crate::ids::{AgentId, RunId, SessionId};
use crate::lifecycle::Phase;
use crate::supervisor::{RunRequest, Supervisor, SupervisorError};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The longest prompt accepted, in UTF-8 bytes.
pub const MAX_PROMPT_BYTES: usize = 8 * 1024;

/// A source of unique ID suffixes.
pub type IdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// Why a click didn't do anything. The message is what the card shows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// No such agent in the corral.
    #[error("There's no agent {0} in the corral.")]
    UnknownAgent(AgentId),
    /// The prompt was empty after trimming.
    #[error("Type a task first.")]
    EmptyPrompt,
    /// The prompt was over [`MAX_PROMPT_BYTES`].
    #[error("That task is too long; keep it under {max_bytes} bytes.")]
    PromptTooLong {
        /// The limit.
        max_bytes: usize,
    },
    /// The agent is working; stop it or wait for it to finish.
    #[error("{0} is still working. Stop it or wait for it to finish.")]
    Busy(AgentId),
    /// Nothing is running to stop.
    #[error("{0} isn't working on anything.")]
    NothingToStop(AgentId),
    /// The agent is already in the corral.
    #[error("{0} is already in the corral.")]
    NothingToSendBack(AgentId),
    /// The agent's last run didn't fail.
    #[error("{0} has no failed task to retry.")]
    NothingToRetry(AgentId),
    /// The supervisor refused.
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
}

#[derive(Debug, Clone)]
struct Latest {
    run: RunId,
    session: SessionId,
    prompt: String,
}

/// Turns clicks on drops into supervisor calls.
pub struct CorralControls {
    supervisor: Supervisor,
    agents: Mutex<HashSet<AgentId>>,
    latest: Mutex<HashMap<AgentId, Latest>>,
    ids: IdSource,
}

impl CorralControls {
    /// Controls for these agents, deploying through `supervisor`.
    pub fn new(
        supervisor: Supervisor,
        agents: impl IntoIterator<Item = AgentId>,
        ids: IdSource,
    ) -> Self {
        Self {
            supervisor,
            agents: Mutex::new(agents.into_iter().collect()),
            latest: Mutex::new(HashMap::new()),
            ids,
        }
    }

    /// Replaces the agents that take tasks (keys connected or removed). A run already going
    /// carries on; Stop, Send back and Retry work only for agents in the corral.
    pub fn set_agents(&self, agents: impl IntoIterator<Item = AgentId>) {
        // A poisoned set refuses every task (`known` fails closed), so there's nothing to do then.
        if let Ok(mut known) = self.agents.lock() {
            *known = agents.into_iter().collect();
        }
    }

    /// Deploys `agent` on `prompt` and returns the new run.
    pub fn deploy(&self, agent: &AgentId, prompt: &str) -> Result<RunId, ControlError> {
        self.known(agent)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(ControlError::EmptyPrompt);
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(ControlError::PromptTooLong {
                max_bytes: MAX_PROMPT_BYTES,
            });
        }
        let mut latest = self.lock()?;
        let session = match latest.get(agent) {
            None => self.new_session(),
            Some(last) => match self.phase(&last.run) {
                Some(Phase::Completed | Phase::Failed) => last.session.clone(),
                Some(Phase::Cancelled) => {
                    self.supervisor.close_session(&last.run)?;
                    self.new_session()
                }
                Some(Phase::Idle) | None => self.new_session(),
                Some(
                    Phase::Queued | Phase::Running | Phase::AwaitingCheckpoint | Phase::Paused,
                ) => return Err(ControlError::Busy(agent.clone())),
            },
        };
        self.start(&mut latest, agent, session, prompt.to_owned())
    }

    /// Reruns `agent`'s failed prompt in the same session.
    pub fn retry(&self, agent: &AgentId) -> Result<RunId, ControlError> {
        self.known(agent)?;
        let mut latest = self.lock()?;
        let Some(last) = latest.get(agent).cloned() else {
            return Err(ControlError::NothingToRetry(agent.clone()));
        };
        if self.phase(&last.run) != Some(Phase::Failed) {
            return Err(ControlError::NothingToRetry(agent.clone()));
        }
        self.start(&mut latest, agent, last.session, last.prompt)
    }

    /// Stops `agent`'s working run. Returns once it has stopped.
    pub async fn stop(&self, agent: &AgentId) -> Result<(), ControlError> {
        self.known(agent)?;
        let run = {
            let latest = self.lock()?;
            match latest.get(agent) {
                Some(last) if self.phase(&last.run).is_some_and(is_working) => last.run.clone(),
                Some(_) | None => return Err(ControlError::NothingToStop(agent.clone())),
            }
        };
        self.supervisor.cancel(&run).await?;
        Ok(())
    }

    /// Closes `agent`'s session once its run has ended.
    pub fn send_back(&self, agent: &AgentId) -> Result<(), ControlError> {
        self.known(agent)?;
        let mut latest = self.lock()?;
        let Some(last) = latest.get(agent) else {
            return Err(ControlError::NothingToSendBack(agent.clone()));
        };
        match self.phase(&last.run) {
            Some(Phase::Completed | Phase::Failed | Phase::Cancelled) => {
                self.supervisor.close_session(&last.run)?;
                latest.remove(agent);
                Ok(())
            }
            Some(Phase::Idle) | None => {
                latest.remove(agent);
                Err(ControlError::NothingToSendBack(agent.clone()))
            }
            Some(Phase::Queued | Phase::Running | Phase::AwaitingCheckpoint | Phase::Paused) => {
                Err(ControlError::Busy(agent.clone()))
            }
        }
    }

    fn start(
        &self,
        latest: &mut HashMap<AgentId, Latest>,
        agent: &AgentId,
        session: SessionId,
        prompt: String,
    ) -> Result<RunId, ControlError> {
        let run = RunId::from(format!("run-{}", (self.ids)()));
        self.supervisor.deploy(RunRequest {
            run_id: run.clone(),
            agent_id: agent.clone(),
            session_id: session.clone(),
            prompt: prompt.clone(),
        })?;
        latest.insert(
            agent.clone(),
            Latest {
                run: run.clone(),
                session,
                prompt,
            },
        );
        Ok(run)
    }

    fn new_session(&self) -> SessionId {
        SessionId::from(format!("session-{}", (self.ids)()))
    }

    fn phase(&self, run: &RunId) -> Option<Phase> {
        self.supervisor.phase(run)
    }

    fn known(&self, agent: &AgentId) -> Result<(), ControlError> {
        let known = self
            .agents
            .lock()
            .map_err(|_| ControlError::Supervisor(SupervisorError::Poisoned))?;
        if known.contains(agent) {
            Ok(())
        } else {
            Err(ControlError::UnknownAgent(agent.clone()))
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<AgentId, Latest>>, ControlError> {
        self.latest
            .lock()
            .map_err(|_| ControlError::Supervisor(SupervisorError::Poisoned))
    }
}

fn is_working(phase: Phase) -> bool {
    match phase {
        Phase::Queued | Phase::Running | Phase::AwaitingCheckpoint | Phase::Paused => true,
        Phase::Idle | Phase::Completed | Phase::Failed | Phase::Cancelled => false,
    }
}
