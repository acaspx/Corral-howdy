//! The filesystem sandbox (M2 item 6, security model T8, TP-FS-01..10). Protected path.
//!
//! An agent's file tools reach only its own folders (its profile's `sandbox_roots`):
//! - **Beneath a handle.** Each root is opened once as a directory handle (`cap_std::fs::Dir`),
//!   and every path is opened beneath it. `..` and links can't lead out, even if the folder
//!   changes after the sandbox was made, because nothing is checked by name and then reopened
//!   (TOCTOU, TP-FS-08). A link inside the folder works when its target is relative and stays
//!   inside; an absolute target is refused even when it points inside.
//! - **The path itself** is refused before anything is opened when it's empty, too long, has a
//!   NUL, starts with `~`, or has a `..` anywhere (TP-FS-01..02). An absolute path must be inside
//!   a root; a relative one is inside the first root.
//! - **Hard links.** A file with more than one link might be a door to a file outside, so it's
//!   neither read, written nor deleted (TP-FS-05).
//! - **Names APFS treats as the same.** A write whose name matches an existing entry only when
//!   case and Unicode normalization are ignored is refused, so `report.md` can't silently replace
//!   `Report.md` on a case-insensitive disk (TP-FS-06..07).
//! - **Whole or nothing.** A write goes to a temp file in the same folder, then is renamed over
//!   the target, so a crash never leaves half a file and a link at the target is never followed.
//!   Reads and writes are capped at 4 MB.
//!
//! The sandbox only places files. Whether an agent may read, write or delete at all is the
//! policy's decision; these tools run only with the gate's token.

use crate::{BoxFuture, Tool, ToolError};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, MetadataExt, OpenOptions};
use corral_core::domain::ActionClass;
use serde_json::Value;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_normalization::UnicodeNormalization;

/// The largest file the sandbox reads.
pub const MAX_READ_BYTES: u64 = 4_194_304;
/// The largest file the sandbox writes.
pub const MAX_WRITE_BYTES: u64 = 4_194_304;
/// The longest path an agent may give.
pub const MAX_PATH_BYTES: usize = 1_024;

/// `EXDEV`, which Linux's `RESOLVE_BENEATH` returns for a path that would leave the folder. The
/// same number on macOS.
const EXDEV: i32 = 18;

/// Why the sandbox refused, in words the agent and the user can read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsError {
    /// Empty, too long, or with a NUL.
    #[error("That path isn't usable: {0}.")]
    BadPath(&'static str),
    /// Starts with `~`.
    #[error("Paths starting with ~ aren't allowed; use a path inside the agent's folder.")]
    HomeShortcut,
    /// Has a `..`.
    #[error("Paths with .. aren't allowed; use a path inside the agent's folder.")]
    ParentDir,
    /// An absolute path outside every root.
    #[error("That path is outside the agent's folders.")]
    Outside,
    /// A link (or a folder swapped for one) leads out.
    #[error("That path leads outside the agent's folder through a link, so Corral refused it.")]
    Escapes,
    /// A file with more than one hard link.
    #[error("That file is also linked from somewhere else, so Corral won't touch it.")]
    Linked,
    /// A name that matches an existing one except for case or normalization.
    #[error(
        "{existing} already exists with a name that differs only in case or accents; use that exact name."
    )]
    NameClash {
        /// The name already there.
        existing: String,
    },
    /// A folder, a link where a file was expected, or something else that isn't a regular file.
    #[error("That isn't a regular file.")]
    NotAFile,
    /// Nothing there.
    #[error("There's no such file or folder.")]
    NotFound,
    /// Over the read or write cap.
    #[error("That's {0} bytes, over the 4 MB limit.")]
    TooLarge(u64),
    /// A sandbox with no folders.
    #[error("The sandbox needs at least one folder.")]
    NoRoots,
    /// A root that isn't an existing folder.
    #[error("{0} isn't a folder Corral can use.")]
    BadRoot(String),
    /// Anything else the system reported.
    #[error("Corral couldn't do that: {0}")]
    Io(String),
}

/// One root: its handle and where it is.
#[derive(Debug)]
struct Root {
    dir: Dir,
    given: PathBuf,
    canonical: PathBuf,
}

/// An agent's folders.
#[derive(Debug)]
pub struct Sandbox {
    roots: Vec<Root>,
}

impl Sandbox {
    /// A sandbox over `roots`, each an existing folder. Relative paths are inside the first.
    pub fn new(roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Result<Self, FsError> {
        let mut opened = Vec::new();
        for given in roots {
            let given: PathBuf = given.into();
            let bad = || FsError::BadRoot(given.display().to_string());
            let canonical = std::fs::canonicalize(&given).map_err(|_| bad())?;
            if !canonical.is_dir() {
                return Err(bad());
            }
            let dir = Dir::open_ambient_dir(&canonical, ambient_authority()).map_err(|_| bad())?;
            opened.push(Root {
                dir,
                given,
                canonical,
            });
        }
        if opened.is_empty() {
            return Err(FsError::NoRoots);
        }
        Ok(Self { roots: opened })
    }

    /// The file at `path`, at most [`MAX_READ_BYTES`].
    pub fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let (dir, rel) = self.locate(path)?;
        // Checked before opening, so a folder or a pipe is never opened for reading.
        if !dir.metadata(&rel).map_err(from_io)?.is_file() {
            return Err(FsError::NotAFile);
        }
        let file = dir.open(&rel).map_err(from_io)?;
        // Checked again on the open file: this is the file that will be read.
        let meta = file.metadata().map_err(from_io)?;
        if !meta.is_file() {
            return Err(FsError::NotAFile);
        }
        if meta.nlink() > 1 {
            return Err(FsError::Linked);
        }
        if meta.len() > MAX_READ_BYTES {
            return Err(FsError::TooLarge(meta.len()));
        }
        let mut bytes = Vec::new();
        file.take(MAX_READ_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(from_io)?;
        let read = bytes.len() as u64;
        if read > MAX_READ_BYTES {
            return Err(FsError::TooLarge(read));
        }
        Ok(bytes)
    }

    /// Writes `contents` to `path`, whole or not at all. The folder must exist.
    pub fn write(&self, path: &str, contents: &[u8]) -> Result<(), FsError> {
        let (dir, rel) = self.locate(path)?;
        let size = contents.len() as u64;
        if size > MAX_WRITE_BYTES {
            return Err(FsError::TooLarge(size));
        }
        let (parent, name) = split(dir, &rel)?;
        match parent.symlink_metadata(&name) {
            Ok(meta) if !meta.is_file() => return Err(FsError::NotAFile),
            Ok(meta) if meta.nlink() > 1 => return Err(FsError::Linked),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(from_io(e)),
        }
        clash(&parent, &name)?;
        let temp = temp_name();
        let written =
            write_new(&parent, &temp, contents).and_then(|()| parent.rename(&temp, &parent, &name));
        if let Err(e) = written {
            // The temp file may or may not exist; either way it isn't the user's.
            let cleaned = parent.remove_file(&temp);
            drop(cleaned);
            return Err(from_io(e));
        }
        Ok(())
    }

    /// Deletes the file at `path` (never a folder). A link is removed, not followed.
    pub fn delete(&self, path: &str) -> Result<(), FsError> {
        let (dir, rel) = self.locate(path)?;
        let (parent, name) = split(dir, &rel)?;
        let meta = parent.symlink_metadata(&name).map_err(from_io)?;
        if meta.is_dir() {
            return Err(FsError::NotAFile);
        }
        if meta.is_file() && meta.nlink() > 1 {
            return Err(FsError::Linked);
        }
        parent.remove_file(&name).map_err(from_io)
    }

    /// The root `path` is in and the path beneath it.
    fn locate(&self, path: &str) -> Result<(&Dir, PathBuf), FsError> {
        if path.is_empty() {
            return Err(FsError::BadPath("it's empty"));
        }
        if path.len() > MAX_PATH_BYTES {
            return Err(FsError::BadPath("it's longer than 1,024 bytes"));
        }
        if path.contains('\0') {
            return Err(FsError::BadPath("it has a NUL character"));
        }
        if path.starts_with('~') {
            return Err(FsError::HomeShortcut);
        }
        let p = Path::new(path);
        if p.components().any(|c| c == Component::ParentDir) {
            return Err(FsError::ParentDir);
        }
        let (root, rest) = if p.is_absolute() {
            self.roots
                .iter()
                .find_map(|r| {
                    p.strip_prefix(&r.canonical)
                        .or_else(|_| p.strip_prefix(&r.given))
                        .ok()
                        .map(|rest| (r, rest))
                })
                .ok_or(FsError::Outside)?
        } else {
            let first = self.roots.first().ok_or(FsError::NoRoots)?;
            (first, p)
        };
        let rel: PathBuf = rest
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .collect();
        let rel = if rel.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            rel
        };
        Ok((&root.dir, rel))
    }
}

/// The folder holding `rel` and its last name.
fn split(dir: &Dir, rel: &Path) -> Result<(Dir, String), FsError> {
    let name = rel
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(FsError::NotAFile)?
        .to_owned();
    let parent = match rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => dir.open_dir(p).map_err(from_io)?,
        _ => dir.try_clone().map_err(from_io)?,
    };
    Ok((parent, name))
}

/// Refuses `name` when an entry differs from it only in case or Unicode normalization.
fn clash(parent: &Dir, name: &str) -> Result<(), FsError> {
    let want = fold(name);
    for entry in parent.entries().map_err(from_io)? {
        let existing = entry
            .map_err(from_io)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if existing != name && fold(&existing) == want {
            return Err(FsError::NameClash { existing });
        }
    }
    Ok(())
}

/// A name as APFS compares it: lowercased, then NFC.
fn fold(name: &str) -> String {
    name.to_lowercase().nfc().collect()
}

fn temp_name() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        ".corral-write-{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Creates `name` (it must not exist) with `contents`, flushed to disk.
fn write_new(parent: &Dir, name: &str, contents: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = parent.open_with(name, &options)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn from_io(e: io::Error) -> FsError {
    let escape = e.raw_os_error() == Some(EXDEV)
        || (e.kind() == io::ErrorKind::PermissionDenied
            && e.to_string().contains("outside of the filesystem"));
    if escape {
        return FsError::Escapes;
    }
    if e.kind() == io::ErrorKind::NotFound {
        FsError::NotFound
    } else {
        FsError::Io(e.to_string())
    }
}

/// `args[key]` as a string, or why not.
fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args[key]
        .as_str()
        .ok_or_else(|| ToolError::BadArgs(format!("`{key}` must be a string")))
}

fn failed(e: FsError) -> ToolError {
    ToolError::Failed(e.to_string())
}

/// `fs.read`: reads a text file in the agent's folder (`read.local`).
pub struct FsRead {
    sandbox: Arc<Sandbox>,
}

impl FsRead {
    /// The tool, over `sandbox`.
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        Self { sandbox }
    }
}

impl Tool for FsRead {
    fn name(&self) -> &str {
        "fs.read"
    }

    fn class(&self) -> ActionClass {
        ActionClass::ReadLocal
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["path"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let bytes = self.sandbox.read(arg(args, "path")?).map_err(failed)?;
            String::from_utf8(bytes).map_err(|_| {
                ToolError::Failed("That file isn't text, so Corral didn't read it.".into())
            })
        })
    }
}

/// `fs.write`: writes a file in the agent's folder (`write.local`).
pub struct FsWrite {
    sandbox: Arc<Sandbox>,
}

impl FsWrite {
    /// The tool, over `sandbox`.
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        Self { sandbox }
    }
}

impl Tool for FsWrite {
    fn name(&self) -> &str {
        "fs.write"
    }

    fn class(&self) -> ActionClass {
        ActionClass::WriteLocal
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["path"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let path = arg(args, "path")?;
            let content = arg(args, "content")?;
            self.sandbox
                .write(path, content.as_bytes())
                .map_err(failed)?;
            Ok(format!("Wrote {} bytes to {path}.", content.len()))
        })
    }
}

/// `fs.delete`: deletes a file in the agent's folder (`delete.local`).
pub struct FsDelete {
    sandbox: Arc<Sandbox>,
}

impl FsDelete {
    /// The tool, over `sandbox`.
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        Self { sandbox }
    }
}

impl Tool for FsDelete {
    fn name(&self) -> &str {
        "fs.delete"
    }

    fn class(&self) -> ActionClass {
        ActionClass::DeleteLocal
    }

    fn destination(&self, args: &Value) -> Option<String> {
        args["path"].as_str().map(str::to_owned)
    }

    fn call<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let path = arg(args, "path")?;
            self.sandbox.delete(path).map_err(failed)?;
            Ok(format!("Deleted {path}."))
        })
    }
}
