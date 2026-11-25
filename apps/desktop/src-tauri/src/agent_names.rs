//! Agents' names (M4 item 6; TP-ANM-03..04): the app's one lookup, so every place that names an
//! agent gives the same name: the corral's cards (with a monogram from the name), the main
//! window's sections, and the agent's folder, `~/Corral/<name>`, its file tools' sandbox. What a
//! name can be is the core's (`corral_core::agent_name`).
//!
//! - **Without a rename,** an agent has its catalog name.
//! - **Fails closed.** A saved name that can't be used, or that two agents share, is dropped:
//!   each keeps its catalog name and its own folder, and the problem says why. No agent can be
//!   given another agent's catalog name, so its default folder stays its own.
//! - **Shared.** Clones hold the same names, so a rename reaches every holder at once.

use crate::agent_settings::AgentSettings;
use crate::agent_tools::agent_folder;
use corral_adapters::catalog::{agent_for, provider_of};
use corral_core::agent_name::AgentName;
use corral_core::corral_view::AgentCard;
use corral_core::ids::AgentId;
use corral_secrets::Provider;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

/// The agents' names: their renames, over the catalog's.
#[derive(Debug, Clone, Default)]
pub struct AgentNames {
    renamed: Arc<RwLock<HashMap<AgentId, AgentName>>>,
}

impl AgentNames {
    /// The names saved in `settings`, and what couldn't be used, one line each.
    pub fn load(settings: &AgentSettings) -> (Self, Vec<String>) {
        let mut problems = Vec::new();
        let mut saved: Vec<(AgentCard, AgentName)> = Vec::new();
        for card in Provider::ALL.into_iter().map(agent_for) {
            match settings.name(&card.id) {
                Ok(Some(name)) => saved.push((card, name)),
                Ok(None) => {}
                Err(why) => problems.push(format!("{} keeps its name: {why}", card.name)),
            }
        }
        let mut renamed = HashMap::new();
        for (i, (card, name)) in saved.iter().enumerate() {
            let sharing: Vec<&str> = saved
                .iter()
                .filter(|(_, other)| name.same_as(other.as_str()))
                .map(|(other, _)| other.name.as_str())
                .collect();
            if sharing.len() == 1 {
                renamed.insert(card.id.clone(), name.clone());
            } else if saved[..i]
                .iter()
                .all(|(_, earlier)| !name.same_as(earlier.as_str()))
            {
                // Said once, by the first of them.
                problems.push(format!(
                    "{} can't both be called {:?}, so each keeps its own name.",
                    sharing.join(" and "),
                    name.as_str()
                ));
            }
        }
        let names = Self {
            renamed: Arc::new(RwLock::new(renamed)),
        };
        (names, problems)
    }

    /// Renames `agent`, or with `None` gives it back its catalog name. A name another agent has,
    /// another agent's catalog name, or an agent that isn't one of Corral's, is refused and
    /// changes nothing.
    pub fn set(&self, agent: &AgentId, name: Option<AgentName>) -> Result<(), String> {
        let mut renamed = self.renamed.write().unwrap_or_else(PoisonError::into_inner);
        check_in(&renamed, agent, name.as_ref())?;
        match name {
            Some(name) => {
                renamed.insert(agent.clone(), name);
            }
            None => {
                renamed.remove(agent);
            }
        }
        Ok(())
    }

    /// Whether [`Self::set`] would take this rename, without making it.
    pub fn check(&self, agent: &AgentId, name: Option<&AgentName>) -> Result<(), String> {
        let renamed = self.renamed.read().unwrap_or_else(PoisonError::into_inner);
        check_in(&renamed, agent, name)
    }

    /// `agent`'s name: its rename, its catalog name, or for an agent Corral doesn't know, its ID.
    pub fn name_of(&self, agent: &AgentId) -> String {
        let renamed = self.renamed.read().unwrap_or_else(PoisonError::into_inner);
        match provider_of(agent) {
            Some(provider) => name_in(&renamed, &agent_for(provider)),
            None => agent.to_string(),
        }
    }

    /// `provider`'s agent's card, with its name and a monogram from it.
    pub fn card(&self, provider: Provider) -> AgentCard {
        self.named(agent_for(provider))
    }

    /// `cards` with each catalog agent's name; a linked app keeps its own.
    pub fn rename(&self, cards: Vec<AgentCard>) -> Vec<AgentCard> {
        cards.into_iter().map(|card| self.named(card)).collect()
    }

    /// `agent`'s folder under `home`: `~/Corral/<name>`.
    pub fn folder(&self, home: &Path, agent: &AgentId) -> PathBuf {
        agent_folder(home, &self.name_of(agent))
    }

    fn named(&self, card: AgentCard) -> AgentCard {
        let renamed = self.renamed.read().unwrap_or_else(PoisonError::into_inner);
        // Only Corral's own agents are ever renamed (`load` and `set`).
        match renamed.get(&card.id) {
            Some(name) => AgentCard {
                name: name.as_str().to_owned(),
                monogram: name.monogram(),
                ..card
            },
            None => card,
        }
    }
}

/// Whether `agent` can be renamed `name` among `renamed`.
fn check_in(
    renamed: &HashMap<AgentId, AgentName>,
    agent: &AgentId,
    name: Option<&AgentName>,
) -> Result<(), String> {
    if provider_of(agent).is_none() {
        return Err(format!(
            "{agent} isn't one of Corral's agents, so it can't be renamed."
        ));
    }
    let Some(name) = name else {
        return Ok(());
    };
    let taken = Provider::ALL
        .into_iter()
        .map(agent_for)
        .filter(|card| card.id != *agent)
        .find(|card| {
            name.same_as(&card.name)
                || renamed
                    .get(&card.id)
                    .is_some_and(|other| name.same_as(other.as_str()))
        });
    match taken {
        Some(card) => Err(format!(
            "{:?} is taken by {}.",
            name.as_str(),
            name_in(renamed, &card)
        )),
        None => Ok(()),
    }
}

/// `card`'s agent's name in `renamed`, or its catalog name.
fn name_in(renamed: &HashMap<AgentId, AgentName>, card: &AgentCard) -> String {
    renamed
        .get(&card.id)
        .map_or_else(|| card.name.clone(), |name| name.as_str().to_owned())
}
