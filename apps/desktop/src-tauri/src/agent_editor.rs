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
//! - **GPT's models are its ChatGPT plan's** (TP-SUB-08): listed in the plan's order with no
//!   price, the saved one shown while the plan lists it, and a save must pick one it lists.
//! - **A folder is never shared.** A rename whose folder already exists is refused unless it's
//!   the agent's own (its catalog name's), so an agent never starts in another agent's old files.

use crate::agent_names::AgentNames;
use crate::agent_settings::{AgentSettings, AgentSetup};
use crate::chatgpt_signin::{ChatGptSignIn, plan_models};
use corral_adapters::catalog::{
    ModelChoice, agent_for, model_choice, models, provider_of, runs_on_chatgpt_plan,
};
use corral_adapters::chatgpt::PlanModel;
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

/// What the ChatGPT plan offers the editor (TP-SUB-08): fetched by the commands, passed in, so
/// the editor's checks stay pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanModels {
    /// No ChatGPT account is signed in.
    SignedOut,
    /// The plan's models, in its order.
    Listed(Vec<PlanModel>),
    /// The plan couldn't be asked, and why.
    Unavailable(String),
}

/// An agent on the ChatGPT plan, as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum PlanState {
    /// Sign in with ChatGPT to choose its model.
    SignedOut,
    /// Its models are the plan's.
    SignedIn,
    /// The plan couldn't be asked.
    Unavailable {
        /// Why, in plain words.
        message: String,
    },
}

/// What a model on the plan shows where a price would be: it spends no dollars.
pub const ON_PLAN: &str = "Uses your ChatGPT plan";

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
    /// Set for an agent that runs on the user's ChatGPT plan (GPT): its models are the plan's,
    /// and it has no dollar cap or spend to show.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chatgpt_plan: Option<PlanState>,
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

    /// Every catalog agent, with no ChatGPT account signed in.
    pub fn views(&self) -> Vec<AgentEditorView> {
        self.views_on_plan(&PlanModels::SignedOut)
    }

    /// Every catalog agent, GPT with what the plan offers.
    pub fn views_on_plan(&self, plan: &PlanModels) -> Vec<AgentEditorView> {
        Provider::ALL
            .into_iter()
            .map(|provider| self.view(provider, plan))
            .collect()
    }

    /// Checks `edit` for `agent` whole, then saves and applies it, with no ChatGPT account
    /// signed in. A refused save changes nothing.
    pub fn save(&self, agent: &str, edit: AgentEdit) -> Result<AgentEditorView, String> {
        self.save_on_plan(agent, edit, &PlanModels::SignedOut)
    }

    /// [`Self::save`], with what the ChatGPT plan offers: GPT's model must be one the plan
    /// lists (TP-SUB-08).
    pub fn save_on_plan(
        &self,
        agent: &str,
        edit: AgentEdit,
        plan: &PlanModels,
    ) -> Result<AgentEditorView, String> {
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
        let on_plan = runs_on_chatgpt_plan(provider);
        let plan_model = match edit.model.as_deref() {
            Some(model) if on_plan => Some(listed(&catalog.name, plan, model)?),
            None | Some(_) => None,
        };
        let model = match edit.model.as_deref() {
            None => models(provider).first(),
            Some(_) if on_plan => None,
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
        if on_plan {
            self.settings
                .save_all_on_plan(&id, &setup, plan_model, name.as_ref())?;
        } else {
            self.settings.save_all(&id, &setup, name.as_ref())?;
        }
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
        Ok(self.view(provider, plan))
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

    fn view(&self, provider: Provider, plan: &PlanModels) -> AgentEditorView {
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
        let (offered, model, chatgpt_plan) = if runs_on_chatgpt_plan(provider) {
            let (offered, state) = match plan {
                PlanModels::SignedOut => (Vec::new(), PlanState::SignedOut),
                PlanModels::Listed(listed) => (
                    listed.iter().map(plan_model_view).collect(),
                    PlanState::SignedIn,
                ),
                PlanModels::Unavailable(message) => (
                    Vec::new(),
                    PlanState::Unavailable {
                        message: message.clone(),
                    },
                ),
            };
            // The saved model while the plan lists it, else the plan's first, as a run uses.
            let saved = self.settings.plan_model(&card.id).ok().flatten();
            let model = saved
                .filter(|slug| offered.iter().any(|m: &ModelView| m.id == *slug))
                .or_else(|| offered.first().map(|m| m.id.clone()));
            (offered, model, Some(state))
        } else {
            (
                models(provider).iter().map(model_view).collect(),
                setup.model.map(|m| m.id.to_owned()),
                None,
            )
        };
        AgentEditorView {
            agent: card.id.as_str().to_owned(),
            name: card.name.clone(),
            catalog_name: catalog.name,
            folder: format!("~/Corral/{}", card.name),
            models: offered,
            model,
            monthly_cap_micro_usd: setup.monthly_cap_micro_usd,
            default_cap_micro_usd: config.agent_month_cap_micro_usd,
            max_cap_micro_usd: config.global_month_cap_micro_usd,
            spent_micro_usd: self
                .ledger
                .get()
                .map(|(ledger, clock)| ledger.spent(&card.id, clock.now_ms())),
            problem: setup_problem.or(name_problem),
            chatgpt_plan,
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

/// `slug` as one of the models `plan` lists for `agent_name`, or why not.
fn listed<'a>(agent_name: &str, plan: &PlanModels, slug: &'a str) -> Result<&'a str, String> {
    match plan {
        PlanModels::Listed(models) if models.iter().any(|m| m.slug == slug) => Ok(slug),
        PlanModels::Listed(_) => Err(format!(
            "{slug:?} isn't a model your ChatGPT plan lists for {agent_name}."
        )),
        PlanModels::SignedOut => Err(format!(
            "{slug:?} isn't a model Corral offers for {agent_name} without a ChatGPT plan. \
             Continue with ChatGPT above, then choose one of its models."
        )),
        PlanModels::Unavailable(why) => Err(format!(
            "Couldn't check {slug:?} against your ChatGPT plan: {why}"
        )),
    }
}

fn plan_model_view(model: &PlanModel) -> ModelView {
    ModelView {
        id: model.slug.clone(),
        name: model.display_name.clone(),
        price: ON_PLAN.to_owned(),
    }
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

/// Every agent, for the main window; GPT with the models the ChatGPT plan lists (TP-SUB-08).
/// Async: asking the plan can take a moment.
#[tauri::command]
pub async fn agent_editor(
    editor: State<'_, Arc<AgentEditor>>,
    signin: State<'_, Arc<ChatGptSignIn>>,
) -> Result<Vec<AgentEditorView>, String> {
    let plan = plan_models(signin.plan()).await;
    Ok(editor.views_on_plan(&plan))
}

/// Saves one agent; it applies without a restart. GPT's model is checked against what the
/// plan lists now (kept per account, so this rarely asks again).
#[tauri::command]
pub async fn save_agent(
    editor: State<'_, Arc<AgentEditor>>,
    signin: State<'_, Arc<ChatGptSignIn>>,
    agent: String,
    edit: AgentEdit,
) -> Result<AgentEditorView, String> {
    let on_plan = provider_of(&AgentId::from(agent.as_str())).is_some_and(runs_on_chatgpt_plan);
    let plan = if on_plan {
        plan_models(signin.plan()).await
    } else {
        PlanModels::SignedOut
    };
    editor.save_on_plan(&agent, edit, &plan)
}
