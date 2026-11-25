//! Each agent's policy, saved (M4 item 6; TP-PED-05..06). The rules are the policy engine's
//! (`corral-policy`); this only keeps them.
//!
//! - **One file per agent**: `<app data>/policies/<agent>.toml`, the profile the policy editor
//!   saved, written through a temporary file and a rename.
//! - **No file means the defaults.**
//! - **Fails closed.** A file that's there but can't be used (unreadable, over
//!   [`MAX_POLICY_FILE_BYTES`], not a valid profile, a rule above a ceiling, a bad domain) is an
//!   error, never the defaults: a broken file must not quietly loosen a rule the user tightened.
//!   The app then gives that agent no tools.
//! - **The editor** ([`PolicyEditor`], TP-PED-08) shows each agent's policy as the core decides
//!   it and saves a form the core checked; a save bumps that agent's version, so its next run is
//!   built under it.
//! - **Plain names only.** An agent ID that isn't letters, digits, `-` and `_` is refused, so a
//!   path never leaves the folder.

use crate::agent_names::AgentNames;
use corral_core::corral_view::AgentCard;
use corral_core::domain::ActionClass;
use corral_core::ids::AgentId;
use corral_policy::{CLASSES, OnTimeout, Policy, Profile, ProfileEdit, Rule, RuleSource};
use corral_secrets::Provider;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::State;

/// The largest policy file read, in bytes.
pub const MAX_POLICY_FILE_BYTES: usize = 64 * 1024;

/// The longest agent ID used as a file name.
const MAX_ID_CHARS: usize = 64;

/// The agents' saved policies, in one folder.
#[derive(Debug, Clone)]
pub struct AgentPolicies {
    dir: PathBuf,
}

impl AgentPolicies {
    /// The policies kept in `dir` (made when the first one is saved).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `agent`'s policy: its saved file under the default ceilings, or the default policy when
    /// it has none. An error says which file can't be used and why.
    pub fn load(&self, agent: &AgentId) -> Result<Policy, String> {
        let path = self.path(agent)?;
        let fail = |why: String| format!("{}: {why}", path.display());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Policy::new(Profile::default()));
            }
            Err(e) => return Err(fail(e.to_string())),
        };
        if bytes.len() > MAX_POLICY_FILE_BYTES {
            return Err(fail(format!(
                "the file is {} bytes; the most is 64 KB",
                bytes.len()
            )));
        }
        let text = String::from_utf8(bytes).map_err(|_| fail("it isn't text".into()))?;
        let profile = Profile::from_toml(&text).map_err(|e| fail(e.to_string()))?;
        Policy::checked(profile).map_err(|e| fail(e.to_string()))
    }

    /// Saves `agent`'s policy: a temporary file, then a rename.
    pub fn save(&self, agent: &AgentId, policy: &Policy) -> Result<(), String> {
        let path = self.path(agent)?;
        let fail = |why: String| format!("Couldn't save {}'s policy: {why}", agent);
        let text = policy
            .profile()
            .to_toml()
            .map_err(|e| fail(e.to_string()))?;
        std::fs::create_dir_all(&self.dir).map_err(|e| fail(e.to_string()))?;
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, text.as_bytes()).map_err(|e| fail(e.to_string()))?;
        if let Err(e) = std::fs::rename(&temp, &path) {
            if let Err(cleanup) = std::fs::remove_file(&temp) {
                eprintln!("corral: couldn't remove {}: {cleanup}", temp.display());
            }
            return Err(fail(e.to_string()));
        }
        Ok(())
    }

    /// `<dir>/<agent>.toml`, for a plain agent ID only.
    fn path(&self, agent: &AgentId) -> Result<PathBuf, String> {
        let id = agent.as_str();
        let plain = !id.is_empty()
            && id.chars().count() <= MAX_ID_CHARS
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !plain {
            return Err(format!("{id:?} isn't an agent Corral keeps a policy for"));
        }
        Ok(Path::new(&self.dir).join(format!("{id}.toml")))
    }
}

// ---- The policy editor (TP-PED-08) ----------------------------------------------------------

/// A rule, as the window sends and shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum RuleChoice {
    /// Never.
    Deny,
    /// Stop at a checkpoint.
    Ask,
    /// Go ahead.
    Allow,
}

impl From<Rule> for RuleChoice {
    fn from(rule: Rule) -> Self {
        match rule {
            Rule::Deny => Self::Deny,
            Rule::Ask => Self::Ask,
            Rule::Allow => Self::Allow,
        }
    }
}

impl From<RuleChoice> for Rule {
    fn from(rule: RuleChoice) -> Self {
        match rule {
            RuleChoice::Deny => Self::Deny,
            RuleChoice::Ask => Self::Ask,
            RuleChoice::Allow => Self::Allow,
        }
    }
}

/// Where a class's decision comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum RuleOrigin {
    /// The default for every agent.
    Default,
    /// The agent's own rule.
    Agent,
    /// The most any agent may have capped it.
    Ceiling,
    /// Always the same.
    Fixed,
}

impl From<RuleSource> for RuleOrigin {
    fn from(source: RuleSource) -> Self {
        match source {
            RuleSource::Default => Self::Default,
            RuleSource::Profile => Self::Agent,
            RuleSource::Ceiling => Self::Ceiling,
            RuleSource::Fixed => Self::Fixed,
        }
    }
}

/// What an unanswered checkpoint does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum TimeoutChoice {
    /// The action is denied and the agent is told.
    Deny,
    /// The run is cancelled.
    CancelRun,
}

impl From<OnTimeout> for TimeoutChoice {
    fn from(on: OnTimeout) -> Self {
        match on {
            OnTimeout::Deny => Self::Deny,
            OnTimeout::CancelRun => Self::CancelRun,
        }
    }
}

impl From<TimeoutChoice> for OnTimeout {
    fn from(on: TimeoutChoice) -> Self {
        match on {
            TimeoutChoice::Deny => Self::Deny,
            TimeoutChoice::CancelRun => Self::CancelRun,
        }
    }
}

/// One action class in the editor: what the agent's policy decides and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PolicyClassView {
    /// The class as the policy names it, e.g. `write.local`.
    pub class: String,
    /// What it does, e.g. `Changing a file in its folder`.
    pub title: String,
    /// What the policy decides.
    pub rule: RuleChoice,
    /// Why.
    pub origin: RuleOrigin,
    /// What it would be without the agent's own rule.
    pub default: RuleChoice,
    /// The most any agent may have.
    pub ceiling: RuleChoice,
    /// The agent's own rule, if it has one.
    pub own: Option<RuleChoice>,
    /// Whether the editor changes it.
    pub editable: bool,
}

/// One agent's policy, as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentPolicyView {
    /// The agent's ID, e.g. `grok`.
    pub agent: String,
    /// Its name, e.g. `Grok`.
    pub agent_name: String,
    /// Every class, in the editor's order.
    pub classes: Vec<PolicyClassView>,
    /// Domains `net.read` may reach without asking.
    pub net_allow: Vec<String>,
    /// How long a checkpoint waits, like `30m`.
    pub checkpoint_timeout: String,
    /// What happens when it isn't answered.
    pub on_timeout: TimeoutChoice,
    /// From this many identical calls in a run on, each asks.
    pub loop_limit: u32,
    /// Why the saved policy can't be used, in plain words; the defaults are shown instead.
    pub problem: Option<String>,
}

/// One rule the editor sets, or clears with `null`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyRuleChange {
    /// The class as the policy names it.
    pub class: String,
    /// Its new rule; `null` goes back to the default.
    pub rule: Option<RuleChoice>,
}

/// The editor's form: rule changes, and the whole of each setting.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPolicyEdit {
    /// Rules to set or clear; the others stay.
    pub rules: Vec<PolicyRuleChange>,
    /// Domains `net.read` may reach without asking.
    pub net_allow: Vec<String>,
    /// Like `30m`, `90s` or `2h`.
    pub checkpoint_timeout: String,
    /// What an unanswered checkpoint does.
    pub on_timeout: TimeoutChoice,
    /// From this many identical calls in a run on, each asks.
    pub loop_limit: u32,
}

/// The policy editor: each catalog agent's saved policy, and a version per agent that a save
/// bumps, so that agent's next run is built under it (TP-PED-07).
#[derive(Debug, Clone)]
pub struct PolicyEditor {
    policies: AgentPolicies,
    versions: Arc<Mutex<HashMap<AgentId, u64>>>,
    names: AgentNames,
}

impl PolicyEditor {
    /// The editor for the policies kept in `dir`.
    pub fn open(dir: PathBuf) -> Self {
        Self {
            policies: AgentPolicies::new(dir),
            versions: Arc::new(Mutex::new(HashMap::new())),
            names: AgentNames::default(),
        }
    }

    /// The editor, naming agents from `names` (TP-ANM-04).
    pub fn named(self, names: AgentNames) -> Self {
        Self { names, ..self }
    }

    /// The saved policies it edits.
    pub fn policies(&self) -> &AgentPolicies {
        &self.policies
    }

    /// How many times `agent`'s policy was saved since launch.
    pub fn version(&self, agent: &AgentId) -> u64 {
        let versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        versions.get(agent).copied().unwrap_or(0)
    }

    /// Every catalog agent's policy.
    pub fn views(&self) -> Vec<AgentPolicyView> {
        Provider::ALL
            .into_iter()
            .map(|provider| {
                let card = self.names.card(provider);
                match self.policies.load(&card.id) {
                    Ok(policy) => view(&card, &policy, None),
                    Err(why) => view(
                        &card,
                        &Policy::new(Profile::default()),
                        Some(broken(&card.name, &why)),
                    ),
                }
            })
            .collect()
    }

    /// Checks `edit` against `agent`'s policy (the defaults if its file can't be used, so a save
    /// replaces it), keeps it and bumps the version. A refused edit changes nothing.
    pub fn save(&self, agent: &str, edit: AgentPolicyEdit) -> Result<AgentPolicyView, String> {
        let card = Provider::ALL
            .into_iter()
            .map(|provider| self.names.card(provider))
            .find(|card| card.id.as_str() == agent)
            .ok_or_else(|| format!("{agent:?} isn't an agent Corral keeps a policy for"))?;
        let mut rules = Vec::with_capacity(edit.rules.len());
        for change in &edit.rules {
            let class = class_named(&change.class)?;
            rules.push((class, change.rule.map(Rule::from)));
        }
        let form = ProfileEdit {
            rules,
            net_allow: edit.net_allow,
            checkpoint_timeout: edit.checkpoint_timeout,
            on_timeout: edit.on_timeout.into(),
            loop_limit: edit.loop_limit,
        };
        // One save at a time, so two never interleave their read and write.
        let mut versions = self.versions.lock().unwrap_or_else(PoisonError::into_inner);
        let current = self
            .policies
            .load(&card.id)
            .unwrap_or_else(|_| Policy::new(Profile::default()));
        let edited = current
            .edited(&form)
            .map_err(|e| sentence(&e.to_string()))?;
        self.policies.save(&card.id, &edited)?;
        *versions.entry(card.id.clone()).or_insert(0) += 1;
        Ok(view(&card, &edited, None))
    }
}

/// `name` as an action class, or why not.
fn class_named(name: &str) -> Result<ActionClass, String> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| format!("{name:?} isn't an action class Corral knows"))
}

/// The class's name as the policy writes it.
fn class_name(class: ActionClass) -> String {
    match serde_json::to_value(class) {
        Ok(serde_json::Value::String(name)) => name,
        // Every class serializes as a string; a debug name is still unique.
        Ok(_) | Err(_) => format!("{class:?}"),
    }
}

/// Why a saved policy can't be used, for the editor.
fn broken(agent: &str, why: &str) -> String {
    format!(
        "{agent}'s saved policy can't be used, so {agent} has no tools ({why}). Saving here replaces it."
    )
}

/// An error from the core, starting with a capital letter.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn view(card: &AgentCard, policy: &Policy, problem: Option<String>) -> AgentPolicyView {
    let profile = policy.profile();
    let form = ProfileEdit::from(profile);
    AgentPolicyView {
        agent: card.id.as_str().to_owned(),
        agent_name: card.name.clone(),
        classes: CLASSES
            .into_iter()
            .map(|class| {
                let e = policy.effective(class);
                PolicyClassView {
                    class: class_name(class),
                    title: corral_policy::title(class).to_owned(),
                    rule: e.rule.into(),
                    origin: e.source.into(),
                    default: e.default.into(),
                    ceiling: e.ceiling.into(),
                    own: profile.rule(class).map(RuleChoice::from),
                    editable: e.editable,
                }
            })
            .collect(),
        net_allow: form.net_allow,
        checkpoint_timeout: form.checkpoint_timeout,
        on_timeout: form.on_timeout.into(),
        loop_limit: form.loop_limit,
        problem,
    }
}

/// Every agent's policy, for the main window.
#[tauri::command]
pub fn agent_policies(editor: State<'_, PolicyEditor>) -> Vec<AgentPolicyView> {
    editor.views()
}

/// Saves one agent's policy; its next run uses it.
#[tauri::command]
pub fn save_agent_policy(
    editor: State<'_, PolicyEditor>,
    agent: String,
    edit: AgentPolicyEdit,
) -> Result<AgentPolicyView, String> {
    editor.save(&agent, edit)
}
