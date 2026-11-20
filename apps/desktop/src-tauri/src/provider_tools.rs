//! Which provider-side tools the user turned on, per agent (M3 item 4.2, TP-PROV-11, 12), kept in
//! Corral's own folder as `provider-tools.json`. What can be turned on is the core's
//! (`corral_adapters::catalog::provider_tools`), and what an agent with a tool may do is the
//! policy's; this only reads, writes and reports the choices.
//!
//! - **Off until turned on.** No file means every tool is off.
//! - **Failing closed.** A file that can't be read (not the JSON Corral writes, over 4 KB, an
//!   unknown tool, a tool an agent isn't offered) leaves every tool off, is left as it was, and the
//!   window says why. A choice that can't be saved doesn't take effect.
//! - **Whole or not at all.** A choice is written to a temporary file, then renamed over the old.

use corral_adapters::agent::ProviderTool;
use corral_adapters::catalog::{self, agent_for};
use corral_adapters::messages::{WEB_SEARCH_MICRO_USD, WEB_SEARCHES_PER_TURN};
use corral_core::ids::AgentId;
use corral_secrets::Provider;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::State;

/// The largest choices file read. The real one is a few dozen bytes.
pub const MAX_PROVIDER_TOOLS_BYTES: usize = 4 * 1024;

/// The tools turned on, by agent ID, as the file holds them.
type Choices = BTreeMap<String, BTreeSet<ProviderTool>>;

/// One switch in the main window: a provider-side tool one agent can be given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderToolChoice {
    /// The agent's ID, e.g. `claude`.
    pub agent: String,
    /// Its name, e.g. `Claude`.
    pub agent_name: String,
    /// The tool.
    pub tool: ProviderTool,
    /// Whose servers it runs on, e.g. `Anthropic`.
    pub runs_on: String,
    /// What one use costs, in micro-USD.
    pub cost_micro_usd: u64,
    /// The most uses in one turn.
    pub per_turn: u32,
    /// Whether it's on.
    pub on: bool,
}

/// What the window shows: every switch, and why the saved choices couldn't be read, if they
/// couldn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderToolsView {
    /// Each tool an agent can be given, in the catalog's order.
    pub choices: Vec<ProviderToolChoice>,
    /// Why every tool is off despite the file, in plain words.
    pub problem: Option<String>,
}

/// The tool's name in a sentence.
fn label(tool: ProviderTool) -> &'static str {
    match tool {
        ProviderTool::WebSearch => "web search",
    }
}

/// What one use of `tool` costs on `provider`'s servers, and the most uses a turn, for the tools
/// the catalog offers.
fn price(provider: Provider, tool: ProviderTool) -> Option<(u64, u32)> {
    match (provider, tool) {
        (Provider::Anthropic, ProviderTool::WebSearch) => {
            Some((WEB_SEARCH_MICRO_USD, WEB_SEARCHES_PER_TURN))
        }
        (Provider::Xai | Provider::OpenAi | Provider::Meta, ProviderTool::WebSearch) => None,
    }
}

/// Every tool an agent can be given: its provider, the agent's ID and name, and the tool.
fn offered() -> Vec<(Provider, AgentId, String, ProviderTool)> {
    Provider::ALL
        .into_iter()
        .flat_map(|provider| {
            let agent = agent_for(provider);
            catalog::provider_tools(provider)
                .iter()
                .map(move |tool| (provider, agent.id.clone(), agent.name.clone(), *tool))
        })
        .collect()
}

/// Whether the agent with ID `agent` can be given `tool`.
fn is_offered(agent: &str, tool: ProviderTool) -> bool {
    offered()
        .iter()
        .any(|(_, id, _, offered)| id.as_str() == agent && *offered == tool)
}

/// Reads the file: nothing on when there's none, an error saying why when it can't be used.
fn load(path: &Path) -> Result<Choices, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Choices::new()),
        Err(e) => return Err(e.to_string()),
    };
    if bytes.len() > MAX_PROVIDER_TOOLS_BYTES {
        return Err(format!(
            "the file is {} bytes; the most is 4 KB",
            bytes.len()
        ));
    }
    let choices: Choices = serde_json::from_slice(&bytes)
        .map_err(|e| format!("it isn't the file Corral writes ({e})"))?;
    for (agent, tools) in &choices {
        for tool in tools {
            if !is_offered(agent, *tool) {
                return Err(format!("it turns on {} for {agent}", label(*tool)));
            }
        }
    }
    Ok(choices)
}

/// Writes `choices` whole: a temporary file next to it, then a rename.
fn save(path: &Path, choices: &Choices) -> Result<(), String> {
    let bytes = serde_json::to_vec(choices).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut temp = path.to_path_buf().into_os_string();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, &bytes).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&temp, path) {
        if let Err(cleanup) = std::fs::remove_file(&temp) {
            eprintln!("corral: couldn't remove {}: {cleanup}", temp.display());
        }
        return Err(e.to_string());
    }
    Ok(())
}

struct Saved {
    choices: Choices,
    problem: Option<String>,
}

/// The choices, as the app holds them. Shared by the window's commands and the agents' switches.
pub struct ProviderTools {
    path: PathBuf,
    saved: Mutex<Saved>,
}

impl ProviderTools {
    /// The choices saved at `path`, or every tool off (and the reason) when they can't be read.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let saved = match load(&path) {
            Ok(choices) => Saved {
                choices,
                problem: None,
            },
            Err(reason) => Saved {
                choices: Choices::new(),
                problem: Some(format!(
                    "Couldn't read your provider tool choices, so they're all off: {reason}."
                )),
            },
        };
        Self {
            path,
            saved: Mutex::new(saved),
        }
    }

    fn saved(&self) -> std::sync::MutexGuard<'_, Saved> {
        // The choices are replaced whole under the lock, so a panic elsewhere can't leave them
        // half-changed.
        self.saved.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every switch and whether it's on.
    pub fn view(&self) -> ProviderToolsView {
        let saved = self.saved();
        let choices = offered()
            .into_iter()
            .filter_map(|(provider, agent, agent_name, tool)| {
                let (cost_micro_usd, per_turn) = price(provider, tool)?;
                let on = saved
                    .choices
                    .get(agent.as_str())
                    .is_some_and(|tools| tools.contains(&tool));
                Some(ProviderToolChoice {
                    agent: agent.to_string(),
                    agent_name,
                    tool,
                    runs_on: provider.display_name().to_owned(),
                    cost_micro_usd,
                    per_turn,
                    on,
                })
            })
            .collect();
        ProviderToolsView {
            choices,
            problem: saved.problem.clone(),
        }
    }

    /// Whether `agent` has `tool` turned on, as a run starting now should see it.
    pub fn is_on(&self, agent: &AgentId, tool: ProviderTool) -> bool {
        self.saved()
            .choices
            .get(agent.as_str())
            .is_some_and(|tools| tools.contains(&tool))
    }

    /// Turns `tool` on or off for `agent`, saved before it takes effect.
    pub fn set(
        &self,
        agent: &str,
        tool: ProviderTool,
        on: bool,
    ) -> Result<ProviderToolsView, String> {
        if !is_offered(agent, tool) {
            let name = catalog::provider_of(&AgentId::from(agent))
                .map(|p| agent_for(p).name)
                .unwrap_or_else(|| "That agent".to_owned());
            return Err(format!("{name} can't be given {} yet.", label(tool)));
        }
        {
            let mut saved = self.saved();
            let mut choices = saved.choices.clone();
            let tools = choices.entry(agent.to_owned()).or_default();
            if on {
                tools.insert(tool);
            } else {
                tools.remove(&tool);
            }
            choices.retain(|_, tools| !tools.is_empty());
            save(&self.path, &choices).map_err(|e| format!("Couldn't save your choice: {e}"))?;
            *saved = Saved {
                choices,
                problem: None,
            };
        }
        Ok(self.view())
    }
}

// Async so file access never blocks the main thread.

/// Command: every provider-side tool switch and whether it's on.
#[tauri::command(async)]
pub fn provider_tools(tools: State<'_, Arc<ProviderTools>>) -> ProviderToolsView {
    tools.view()
}

/// Command: turns a provider-side tool on or off for an agent.
#[tauri::command(async)]
pub fn set_provider_tool(
    tools: State<'_, Arc<ProviderTools>>,
    agent: String,
    tool: ProviderTool,
    on: bool,
) -> Result<ProviderToolsView, String> {
    tools.set(&agent, tool, on)
}
