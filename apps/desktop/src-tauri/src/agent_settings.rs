//! Each agent's settings, saved (M4 item 6; TP-AED-03..04, TP-ANM-02): the model it uses, its
//! own monthly cap and its name. What can be chosen is the core's: the models are the catalog's, each at its
//! checked price (`corral_adapters::catalog::models`), and a cap is held by the spend ledger,
//! which refuses one above the all-agents cap. This only keeps the choices.
//!
//! - **One file per agent**: `<app data>/agents/<agent>.toml`, written through a temporary file
//!   and a rename.
//! - **No file means the defaults**: the agent's default model and the default cap.
//! - **Fails closed.** A file that's there but can't be used (unreadable, over
//!   [`MAX_SETTINGS_FILE_BYTES`], not the format, an unknown key, a model Corral doesn't offer that
//!   agent) is an error naming the file, never the defaults. At launch that agent's monthly cap
//!   is then $0 ([`seed_caps`]), so a broken file never quietly raises a cap the user lowered.
//! - **Plain names only.** An agent ID that isn't letters, digits, `-` and `_` is refused, so a
//!   path never leaves the folder.

use corral_adapters::catalog::{ModelChoice, agent_for, model_choice, models, provider_of};
use corral_adapters::messages::MessagesConfig;
use corral_adapters::responses::ResponsesConfig;
use corral_core::agent_name::AgentName;
use corral_core::budget::SpendLedger;
use corral_core::ids::AgentId;
use corral_secrets::Provider;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The largest settings file read, in bytes.
pub const MAX_SETTINGS_FILE_BYTES: usize = 4 * 1024;

/// The longest agent ID used as a file name.
const MAX_ID_CHARS: usize = 64;

/// One agent's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentSetup {
    /// The model it uses; none for an agent whose provider has no client yet.
    pub model: Option<&'static ModelChoice>,
    /// Its own monthly cap, or `None` for the default.
    pub monthly_cap_micro_usd: Option<u64>,
}

/// The file, as it's written.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SetupFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monthly_cap_micro_usd: Option<u64>,
}

/// The agents' saved settings, in one folder.
#[derive(Debug, Clone)]
pub struct AgentSettings {
    dir: PathBuf,
}

impl AgentSettings {
    /// The settings kept in `dir` (made when the first one is saved).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `agent`'s settings: its saved file, or the defaults when it has none. An error says which
    /// file can't be used and why.
    pub fn load(&self, agent: &AgentId) -> Result<AgentSetup, String> {
        let (path, file) = self.read(agent)?;
        let Some(file) = file else {
            return Ok(defaults(agent));
        };
        let model = match file.model {
            None => defaults(agent).model,
            Some(id) => {
                Some(offered(agent, &id).map_err(|why| format!("{}: {why}", path.display()))?)
            }
        };
        Ok(AgentSetup {
            model,
            monthly_cap_micro_usd: file.monthly_cap_micro_usd,
        })
    }

    /// `agent`'s saved name (TP-ANM-02), if it was renamed. An error says which file can't be
    /// used and why, including a name that is another agent's catalog name.
    pub fn name(&self, agent: &AgentId) -> Result<Option<AgentName>, String> {
        let (path, file) = self.read(agent)?;
        let Some(text) = file.and_then(|f| f.name) else {
            return Ok(None);
        };
        let fail = |why: String| format!("{}: {why}", path.display());
        let name = AgentName::new(&text).map_err(|e| fail(e.to_string()))?;
        not_anothers(agent, &name).map_err(fail)?;
        Ok(Some(name))
    }

    /// Saves `agent`'s model and cap, keeping its saved name: a temporary file, then a rename.
    /// A file that can't be read is replaced.
    pub fn save(&self, agent: &AgentId, setup: &AgentSetup) -> Result<(), String> {
        let name = match self.read(agent) {
            Ok((_, Some(file))) => file.name,
            Ok((_, None)) | Err(_) => None,
        };
        self.write(
            agent,
            &SetupFile {
                name,
                model: setup.model.map(|m| m.id.to_owned()),
                monthly_cap_micro_usd: setup.monthly_cap_micro_usd,
            },
        )
    }

    /// Saves `agent`'s name, or `None` to go back to its catalog name, keeping its model and
    /// cap. Another agent's catalog name is refused, and so is a file that can't be read: it may
    /// hold a cap the user lowered.
    pub fn save_name(&self, agent: &AgentId, name: Option<&AgentName>) -> Result<(), String> {
        if let Some(name) = name {
            not_anothers(agent, name)?;
        }
        let (_, file) = self.read(agent)?;
        let file = SetupFile {
            name: name.map(|n| n.as_str().to_owned()),
            ..file.unwrap_or_default()
        };
        self.write(agent, &file)
    }

    /// The file's path, and what it holds if it's there. An error says which file can't be used.
    fn read(&self, agent: &AgentId) -> Result<(PathBuf, Option<SetupFile>), String> {
        let path = self.path(agent)?;
        let fail = |why: String| format!("{}: {why}", path.display());
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((path, None)),
            Err(e) => return Err(fail(e.to_string())),
        };
        if bytes.len() > MAX_SETTINGS_FILE_BYTES {
            return Err(fail(format!(
                "the file is {} bytes; the most is 4 KB",
                bytes.len()
            )));
        }
        let text = String::from_utf8(bytes).map_err(|_| fail("it isn't text".into()))?;
        // The full text quotes the line it stopped at, folded onto one line.
        let file: SetupFile = toml::from_str(&text).map_err(|e| {
            fail(
                e.to_string()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })?;
        Ok((path, Some(file)))
    }

    /// Writes `file` for `agent`: a temporary file, then a rename.
    fn write(&self, agent: &AgentId, file: &SetupFile) -> Result<(), String> {
        let path = self.path(agent)?;
        let fail = |why: String| format!("Couldn't save {agent}'s settings: {why}");
        let text = toml::to_string(file).map_err(|e| fail(e.to_string()))?;
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
            return Err(format!("{id:?} isn't an agent Corral keeps settings for"));
        }
        Ok(Path::new(&self.dir).join(format!("{id}.toml")))
    }
}

/// Refuses `name` for `agent` if it is another agent's catalog name, so every agent's default
/// folder stays its own.
fn not_anothers(agent: &AgentId, name: &AgentName) -> Result<(), String> {
    match Provider::ALL
        .into_iter()
        .map(agent_for)
        .find(|card| card.id != *agent && name.same_as(&card.name))
    {
        Some(card) => Err(format!("{:?} is {}'s name", name.as_str(), card.name)),
        None => Ok(()),
    }
}

/// The defaults: the agent's default model, and the default cap.
fn defaults(agent: &AgentId) -> AgentSetup {
    AgentSetup {
        model: provider_of(agent).and_then(|p| models(p).first()),
        monthly_cap_micro_usd: None,
    }
}

/// `id` as one of the models Corral offers `agent`, or why not.
fn offered(agent: &AgentId, id: &str) -> Result<&'static ModelChoice, String> {
    let Some(provider) = provider_of(agent) else {
        return Err(format!("{id:?} isn't a model Corral offers for {agent}"));
    };
    let name = agent_for(provider).name;
    model_choice(provider, id)
        .ok_or_else(|| format!("{id:?} isn't a model Corral offers for {name}"))
}

/// Gives each of `agents` its saved monthly cap. A file that can't be used, or a cap above the
/// all-agents cap, leaves that agent at $0 (fail closed); the problems say why, one per agent.
pub fn seed_caps(
    ledger: &SpendLedger,
    settings: &AgentSettings,
    agents: &[AgentId],
) -> Vec<String> {
    let mut problems = Vec::new();
    for agent in agents {
        let name = provider_of(agent).map_or_else(|| agent.to_string(), |p| agent_for(p).name);
        let wanted = settings.load(agent).and_then(|setup| {
            ledger
                .set_agent_cap(agent, setup.monthly_cap_micro_usd)
                .map_err(|e| e.to_string())
        });
        if let Err(why) = wanted {
            // $0 is at most the all-agents cap, so this always takes.
            if let Err(e) = ledger.set_agent_cap(agent, Some(0)) {
                eprintln!("corral: couldn't stop {name}'s spending: {e}");
            }
            problems.push(format!(
                "{name} can't spend until its settings are fixed: {why}"
            ));
        }
    }
    problems
}

/// Grok's client, for `model` at its price.
pub fn responses_config(model: &ModelChoice) -> ResponsesConfig {
    ResponsesConfig {
        price: model.price,
        ..ResponsesConfig::xai(model.id)
    }
}

/// Claude's client, for `model` at its price.
pub fn messages_config(model: &ModelChoice) -> MessagesConfig {
    MessagesConfig {
        price: model.price,
        ..MessagesConfig::anthropic(model.id)
    }
}
