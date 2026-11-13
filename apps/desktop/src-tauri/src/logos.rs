//! Where logos live: Corral's own folder, `logos/<name>.png` or `.ico` (ADR-015, docs/07
//! §8.1). Names come from the [`LogoId`], never from the server. A logo is checked
//! again before it's written, written to a temp file and renamed, so Swift never sees half a
//! file. The fetch and the checks are in `corral_adapters::logos`.

use corral_adapters::logos::{
    Logo, LogoError, LogoFetcher, LogoFormat, LogoId, check_logo, fetch_logo,
};
use std::path::{Path, PathBuf};

/// Why a logo wasn't saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogoStoreError {
    /// Fetching or checking it failed.
    #[error(transparent)]
    Fetch(#[from] LogoError),
    /// The folder or file couldn't be written.
    #[error("couldn't save the logo: {0}")]
    Io(String),
}

/// The logos folder.
#[derive(Debug, Clone)]
pub struct LogoStore {
    dir: PathBuf,
}

const FORMATS: [LogoFormat; 2] = [LogoFormat::Png, LogoFormat::Ico];

impl LogoStore {
    /// The store in `dir`, created on the first save.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, id: LogoId, format: LogoFormat) -> PathBuf {
        self.dir
            .join(format!("{}.{}", id.name(), format.extension()))
    }

    /// The saved logo, if there's one: a regular file, not a link.
    pub fn path(&self, id: impl Into<LogoId>) -> Option<PathBuf> {
        let id = id.into();
        FORMATS
            .into_iter()
            .map(|format| self.file(id, format))
            .find(|path| std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()))
    }

    /// Checks and saves a logo, replacing the old one whatever its format.
    pub fn save(&self, logo: &Logo) -> Result<PathBuf, LogoStoreError> {
        if check_logo(&logo.bytes)? != logo.format {
            // The extension must say what's inside.
            return Err(LogoError::NotAnImage.into());
        }
        let io = |what: &str, e: std::io::Error| LogoStoreError::Io(format!("{what}: {e}"));
        std::fs::create_dir_all(&self.dir).map_err(|e| io("creating the logos folder", e))?;
        let path = self.file(logo.id, logo.format);
        let temp = self.dir.join(format!(
            ".{}.{}.tmp",
            logo.id.name(),
            logo.format.extension()
        ));
        if let Err(e) = std::fs::write(&temp, &logo.bytes) {
            // A leftover temp file is never read (it isn't a logo name), but say so.
            let left = match std::fs::remove_file(&temp) {
                Ok(()) => String::new(),
                Err(c) if c.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(c) => format!(" (a temp file stayed behind: {c})"),
            };
            return Err(LogoStoreError::Io(format!("writing the logo: {e}{left}")));
        }
        std::fs::rename(&temp, &path).map_err(|e| io("moving the logo into place", e))?;
        for format in FORMATS {
            if format == logo.format {
                continue;
            }
            match std::fs::remove_file(self.file(logo.id, format)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io("removing the old logo", e)),
            }
        }
        Ok(path)
    }
}

/// Each logo: the saved one, or else fetched and saved. Logos with no known source are left
/// out.
pub async fn fetch_missing(
    fetcher: &dyn LogoFetcher,
    store: &LogoStore,
    ids: &[LogoId],
) -> Vec<(LogoId, Result<PathBuf, LogoStoreError>)> {
    let mut results = Vec::new();
    for &id in ids {
        if let Some(path) = store.path(id) {
            results.push((id, Ok(path)));
            continue;
        }
        match fetch_logo(fetcher, id).await {
            Ok(logo) => results.push((id, store.save(&logo))),
            Err(LogoError::NoSource) => {}
            Err(e) => results.push((id, Err(e.into()))),
        }
    }
    results
}
