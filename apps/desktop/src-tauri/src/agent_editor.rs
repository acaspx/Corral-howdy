//! The agent editor's core (M4 item 6; TP-AED-05..08): for each agent, its name and folder, its
//! model from the priced table, its own monthly cap, and what it spent this month. The rules are
//! the core's and the catalog's (`corral_core::agent_name`, `catalog::models`, the spend
//! ledger's caps); this checks a save against all of them before writing anything.
//!
//! - **Checked whole.** A refused save says why and changes nothing: no file, no name, no cap,
//!   no version.
//! - **One write**, then applied without a restart: the cap from the agent's next request, the
//!   name on its card at once (the corral is told), and the model and folder from its next task
//!   (the agent's version, which its driver is rebuilt on).
//! - **A folder is never shared.** A rename whose folder already exists is refused unless it's
//!   the agent's own (its catalog name's), so an agent never starts in another agent's old files.

use crate::agent_names::AgentNames;
use crate::agent_settings::{AgentSettings, AgentSetup};
use corral_adapters::catalog::{ModelChoice, agent_for, model_choice, models, provider_of};
use corral_core::agent_name::AgentName;
use corral_core::budget::{LedgerConfig, Price, SpendLedger, usd};
use corral_core::clock::Clock;
use corral_core::ids::AgentId;
use corral_secrets::Provider;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use tauri::State;

/// A model the agent can be given, for the editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    /// The model ID the provider's API takes.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its list price, in words.
    pub price: String,
}

/// One agent, as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentEditorView {
    /// The agent's ID, e.g. `claude`.
    pub agent: String,
    /// Its name now.
    pub name: String,
    /// Its catalog name, which an empty name goes back to.
    pub catalog_name: String,
    /// Its folder, like `~/Corral/Claude`.
    pub folder: String,
    /// The models it can be given, its default first; none for an agent Corral can't run yet.
    pub models: Vec<ModelView>,
    /// Its model.
    pub model: Option<String>,
    /// Its own monthly cap, or `null` for the default.
    #[ts(type = "number | null")]
    pub monthly_cap_micro_usd: Option<u64>,
    /// The default monthly cap.
    #[ts(type = "number")]
    pub default_cap_micro_usd: u64,
    /// The most a cap can be: the cap for all agents together.
    #[ts(type = "number")]
    pub max_cap_micro_usd: u64,
    /// What it spent this month, once the corral is running.
    #[ts(type = "number | null")]
    pub spent_micro_usd: Option<u64>,
    /// Why its saved settings or name can't be used, in plain words.
    pub problem: Option<String>,
}

/// The editor's form.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentEdit {
    /// Its name; `null` or blank goes back to its catalog name.
    pub name: Option<String>,
    /// Its model; `null` is its default.
    pub model: Option<String>,
    /// Its own monthly cap; `null` is the default.
    #[ts(type = "number | null")]
    pub monthly_cap_micro_usd: Option<u64>,
}

/// Told after a save, so the corral shows the agent's name.
pub type OnChange = Box<dyn Fn() + Send + Sync>;

/// The agent editor.
pub struct AgentEditor {
    settings: AgentSettings,
    names: AgentNames,
    home: Option<PathBuf>,
    ledger: OnceLock<(Arc<SpendLedger>, Arc<dyn Clock>)>,
    on_change: OnceLock<OnChange>,
    /// Saves since launch, by agent; one save at a time.
    versions: Mutex<HashMap<AgentId, u64>>,
}

impl AgentEditor {
    /// The editor over `settings` and `names`, with agents' folders under `home`.
    pub fn open(settings: AgentSettings, names: AgentNames, home: Option<PathBuf>) -> Self {
        Self {
            settings,
            names,
            home,
            ledger: OnceLock::new(),
            on_change: OnceLock::new(),
            versions: Mutex::new(HashMap::new()),
        }
    }

    /// The running corral's ledger, which a saved cap goes into, and its clock. The first stays.
    pub fn attach(&self, ledger: Arc<SpendLedger>, clock: Arc<dyn Clock>) {
        if self.ledger.set((ledger, clock)).is_err() {
            eprintln!("corral: the agent editor already has a ledger");
        }
    }

    /// What to tell after a save. The first stays.
    pub fn on_change(&self, hook: OnChange) {
        if self.on_change.set(hook).is_err() {
            eprintln!("corral: the agent editor already has a corral to tell");
        }
    }

    /// How many times `agent` was saved since launch.
    pub fn version(&self, agent: &AgentId) -> u64 {
        let versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        versions.get(agent).copied().unwrap_or(0)
    }

    /// Every catalog agent.
    pub fn views(&self) -> Vec<AgentEditorView> {
        Provider::ALL
            .into_iter()
            .map(|provider| self.view(provider))
            .collect()
    }

    /// Checks `edit` for `agent` whole, then saves and applies it. A refused save changes
    /// nothing.
    pub fn save(&self, agent: &str, edit: AgentEdit) -> Result<AgentEditorView, String> {
        let id = AgentId::from(agent);
        let Some(provider) = provider_of(&id) else {
            return Err(format!("{agent:?} isn't one of Corral's agents."));
        };
        let catalog = agent_for(provider);
        // One save at a time, so two never interleave their checks and writes.
        let mut versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);

        let name = match edit.name.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => {
                let name = AgentName::new(text).map_err(|e| e.to_string())?;
                (!name.same_as(&catalog.name)).then_some(name)
            }
        };
        self.names.check(&id, name.as_ref())?;
        if let Some(name) = &name {
            self.folder_is_free(&id, name)?;
        }
        let model = match edit.model.as_deref() {
            None => models(provider).first(),
            Some(model) => Some(offered(provider, model)?),
        };
        let all_agents = self.ledger_config().global_month_cap_micro_usd;
        if let Some(cap) = edit.monthly_cap_micro_usd
            && cap > all_agents
        {
            return Err(format!(
                "A monthly cap can be at most {}, the cap for all agents together.",
                usd(all_agents)
            ));
        }

        let setup = AgentSetup {
            model,
            monthly_cap_micro_usd: edit.monthly_cap_micro_usd,
        };
        self.settings.save_all(&id, &setup, name.as_ref())?;
        if let Err(e) = self.names.set(&id, name) {
            // Checked above under the same lock, so this can't happen; the file holds it for
            // the next launch either way.
            eprintln!("corral: {e}");
        }
        if let Some((ledger, _)) = self.ledger.get()
            && let Err(e) = ledger.set_agent_cap(&id, setup.monthly_cap_micro_usd)
        {
            eprintln!("corral: {e}");
        }
        *versions.entry(id).or_insert(0) += 1;
        drop(versions);
        if let Some(hook) = self.on_change.get() {
            hook();
        }
        Ok(self.view(provider))
    }

    /// Refuses `name` for `agent` if its folder is there and isn't the agent's own: it may hold
    /// another agent's files.
    fn folder_is_free(&self, agent: &AgentId, name: &AgentName) -> Result<(), String> {
        if name.same_as(&self.names.name_of(agent)) {
            // The folder it's in now (macOS names ignore case).
            return Ok(());
        }
        let Some(home) = &self.home else {
            // Fail closed: without the home folder, there's no checking that it's free.
            return Err(format!(
                "Corral can't find your home folder, so it can't check that ~/Corral/{name} is \
                 free."
            ));
        };
        let folder = crate::agent_tools::agent_folder(home, name.as_str());
        match std::fs::symlink_metadata(&folder) {
            Ok(_) => Err(format!(
                "~/Corral/{name} already exists, and it may hold another agent's files. Pick \
                 another name, or move that folder first."
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Couldn't check ~/Corral/{name}: {e}")),
        }
    }

    fn ledger_config(&self) -> LedgerConfig {
        self.ledger
            .get()
            .map_or_else(LedgerConfig::default, |(ledger, _)| ledger.config())
    }

    fn view(&self, provider: Provider) -> AgentEditorView {
        let card = self.names.card(provider);
        let catalog = agent_for(provider);
        let config = self.ledger_config();
        let (setup, setup_problem) = match self.settings.load(&card.id) {
            Ok(setup) => (setup, None),
            Err(why) => (
                AgentSetup {
                    model: models(provider).first(),
                    monthly_cap_micro_usd: None,
                },
                Some(format!(
                    "{} can't spend until its settings are fixed ({why}). Saving here replaces \
                     them.",
                    card.name
                )),
            ),
        };
        let name_problem = self.settings.name(&card.id).err();
        AgentEditorView {
            agent: card.id.as_str().to_owned(),
            name: card.name.clone(),
            catalog_name: catalog.name,
            folder: format!("~/Corral/{}", card.name),
            models: models(provider).iter().map(model_view).collect(),
            model: setup.model.map(|m| m.id.to_owned()),
            monthly_cap_micro_usd: setup.monthly_cap_micro_usd,
            default_cap_micro_usd: config.agent_month_cap_micro_usd,
            max_cap_micro_usd: config.global_month_cap_micro_usd,
            spent_micro_usd: self
                .ledger
                .get()
                .map(|(ledger, clock)| ledger.spent(&card.id, clock.now_ms())),
            problem: setup_problem.or(name_problem),
        }
    }
}

/// `id` as one of the models Corral offers `provider`'s agent, or why not.
fn offered(provider: Provider, id: &str) -> Result<&'static ModelChoice, String> {
    model_choice(provider, id).ok_or_else(|| {
        format!(
            "{id:?} isn't a model Corral offers for {}.",
            agent_for(provider).name
        )
    })
}

fn model_view(model: &ModelChoice) -> ModelView {
    ModelView {
        id: model.id.to_owned(),
        name: model.name.to_owned(),
        price: price_text(&model.price),
    }
}

/// A price per million tokens, in words, with its long-prompt rate if it has one.
pub fn price_text(price: &Price) -> String {
    let mut text = format!(
        "{} in and {} out per million tokens",
        usd(price.input_micro_usd_per_mtok),
        usd(price.output_micro_usd_per_mtok)
    );
    if let Some(long) = price.long_prompt {
        text.push_str(&format!(
            "; {} and {} from {} prompt tokens",
            usd(long.input_micro_usd_per_mtok),
            usd(long.output_micro_usd_per_mtok),
            thousands(long.from_input_tokens)
        ));
    }
    text
}

/// `n` with thousands separators: 200000 is "200,000".
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Every agent, for the main window.
#[tauri::command]
pub fn agent_editor(editor: State<'_, Arc<AgentEditor>>) -> Vec<AgentEditorView> {
    editor.views()
}

/// Saves one agent; it applies without a restart.
#[tauri::command]
pub fn save_agent(
    editor: State<'_, Arc<AgentEditor>>,
    agent: String,
    edit: AgentEdit,
) -> Result<AgentEditorView, String> {
    editor.save(&agent, edit)
}
