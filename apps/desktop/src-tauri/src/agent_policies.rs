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
//! - **Plain names only.** An agent ID that isn't letters, digits, `-` and `_` is refused, so a
//!   path never leaves the folder.

use corral_core::ids::AgentId;
use corral_policy::{Policy, Profile};
use std::path::{Path, PathBuf};

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
