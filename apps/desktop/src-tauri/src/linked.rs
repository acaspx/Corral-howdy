//! Which linked apps show (ADR-005), kept in Corral's own folder as `linked-apps.json`. The core
//! decides what the choices mean (`corral_adapters::linked`); this only reads and writes them,
//! whole or not at all.

use corral_adapters::linked::LinkedSettings;
use std::path::PathBuf;

/// The largest settings file read. The real one is a few dozen bytes.
pub const MAX_LINKED_BYTES: usize = 4 * 1024;

/// Why the settings couldn't be read or saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkedStoreError {
    /// The file is larger than [`MAX_LINKED_BYTES`].
    #[error("the linked apps file is {0} bytes; the most is 4 KB")]
    TooLarge(usize),
    /// The file isn't the settings Corral writes.
    #[error("the linked apps file couldn't be read: {0}")]
    Malformed(String),
    /// The file system refused.
    #[error("couldn't read or save the linked apps file: {0}")]
    Io(String),
}

/// The settings file.
#[derive(Debug, Clone)]
pub struct LinkedStore {
    path: PathBuf,
}

impl LinkedStore {
    /// The store at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The saved choices; nothing linked when there's no file yet.
    pub fn load(&self) -> Result<LinkedSettings, LinkedStoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LinkedSettings::default());
            }
            Err(e) => return Err(LinkedStoreError::Io(e.to_string())),
        };
        if bytes.len() > MAX_LINKED_BYTES {
            return Err(LinkedStoreError::TooLarge(bytes.len()));
        }
        serde_json::from_slice(&bytes).map_err(|e| LinkedStoreError::Malformed(e.to_string()))
    }

    /// Saves the choices: a temp file next to it, then a rename.
    pub fn save(&self, settings: &LinkedSettings) -> Result<(), LinkedStoreError> {
        let io = |e: std::io::Error| LinkedStoreError::Io(e.to_string());
        let bytes =
            serde_json::to_vec(settings).map_err(|e| LinkedStoreError::Malformed(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut temp = self.path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, &bytes).map_err(io)?;
        std::fs::rename(&temp, &self.path).map_err(io)
    }
}
