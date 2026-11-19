//! An agent's tools in the app (M3 item 3.4, TP-TOOL-13..14): wiring only. What each call may do
//! is the policy's decision (`corral-policy`), and where files may go is the sandbox's
//! (`corral-tools::fs`).
//!
//! - **Its own folder.** Each agent works in `~/Corral/<Name>` (security model: "Sandbox root per
//!   agent"), made if it's missing. If it can't be made, the agent gets no tools.
//! - **The file tools** (`fs.read`, `fs.write`, `fs.delete`) over that folder, under the default
//!   policy: reading is allowed, writing asks, deleting asks at high risk.
//! - **One desk.** Checkpoints wait on the app's desk, where the card's answers go.
//! - **Random IDs.** A checkpoint's ID is what makes an answer single use, so IDs come from the
//!   operating system's random source (128 bits).

use corral_adapters::agent::ToolSetup;
use corral_policy::{Policy, Profile};
use corral_tools::fs::{FsDelete, FsRead, FsWrite, Sandbox};
use corral_tools::{CheckpointDesk, IdSource, Tool, ToolGateway};
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The folder an agent named `name` works in: `<home>/Corral/<name>`.
pub fn agent_folder(home: &Path, name: &str) -> PathBuf {
    home.join("Corral").join(name)
}

/// The file tools over `folder` (made if missing), under the default policy, with checkpoints on
/// `desk` and random IDs. An error says which folder couldn't be used.
pub fn file_tools(folder: &Path, desk: Arc<CheckpointDesk>) -> Result<ToolSetup, String> {
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("couldn't make {}: {e}", folder.display()))?;
    let sandbox = Arc::new(
        Sandbox::new([folder.to_path_buf()])
            .map_err(|e| format!("couldn't use {}: {e}", folder.display()))?,
    );
    let tools: [Arc<dyn Tool>; 3] = [
        Arc::new(FsRead::new(Arc::clone(&sandbox))),
        Arc::new(FsWrite::new(Arc::clone(&sandbox))),
        Arc::new(FsDelete::new(sandbox)),
    ];
    let mut gateway = ToolGateway::new();
    for tool in tools {
        gateway
            .register(tool)
            .map_err(|e| format!("couldn't set up the file tools: {e}"))?;
    }
    Ok(ToolSetup {
        policy: Policy::new(Profile::default()),
        gateway: Arc::new(gateway),
        desk,
        ids: random_ids(),
    })
}

/// 128 random bits as 32 lowercase hex characters, from the operating system.
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // The OS source doesn't fail on macOS. If it ever did, fall back to two hashes keyed
        // from the OS's randomness at startup, which are still unguessable from outside.
        let keyed = RandomState::new();
        let (a, b) = (keyed.hash_one(1u8), keyed.hash_one(2u8));
        bytes[..8].copy_from_slice(&a.to_le_bytes());
        bytes[8..].copy_from_slice(&b.to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A source of [`random_id`]s, for tool calls and checkpoints.
pub fn random_ids() -> IdSource {
    Arc::new(random_id)
}
