//! Grok's tools in the app (M3 item 3.4, TP-TOOL-13..14): the file tools over Grok's own folder,
//! `~/Corral/Grok`, under the default policy, with checkpoints on the app's one desk and IDs from
//! the operating system's random source.

use corral_core::domain::{ActionClass, Risk};
use corral_desktop_lib::agent_tools::{agent_folder, file_tools, random_id, random_ids};
use corral_policy::{Action, Gate, ToolCall, Verdict};
use corral_tools::CheckpointDesk;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh stand-in for the user's home folder, cleaned up on drop.
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-home-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear");
        }
        std::fs::create_dir_all(&path).expect("home");
        Self(path)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder only costs disk space.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn verdict(gate: &Gate, class: ActionClass, tool: &str, args: Value) -> Verdict {
    let path = args["path"].as_str().unwrap_or_default().to_owned();
    gate.check(ToolCall {
        action: Action::new(class, tool).to(path),
        args,
    })
}

#[test]
fn tp_tool_13_grok_works_in_its_own_folder() {
    let home = Home::new();
    let folder = agent_folder(&home.0, "Grok");
    assert_eq!(folder, home.0.join("Corral").join("Grok"));
    assert!(!folder.exists());
    let desk = Arc::new(CheckpointDesk::default());
    let setup = file_tools(&folder, Arc::clone(&desk)).expect("tools");
    assert!(folder.is_dir(), "the folder is made");
    assert!(
        Arc::ptr_eq(&setup.desk, &desk),
        "checkpoints wait on the app's desk"
    );
    let offered: Vec<String> = setup
        .gateway
        .offered()
        .into_iter()
        .map(|t| t.wire_name)
        .collect();
    assert_eq!(offered, ["fs_delete", "fs_read", "fs_write"]);
    assert_eq!(
        setup.gateway.class_of("fs.read"),
        Some(ActionClass::ReadLocal)
    );
    assert_eq!(
        setup.gateway.class_of("fs.write"),
        Some(ActionClass::WriteLocal)
    );
    assert_eq!(
        setup.gateway.class_of("fs.delete"),
        Some(ActionClass::DeleteLocal)
    );
    // It works again with the folder already there.
    file_tools(&folder, desk).expect("again");
}

#[test]
fn tp_tool_13_the_default_policy_reads_freely_and_asks_to_change_anything() {
    let home = Home::new();
    let setup = file_tools(
        &agent_folder(&home.0, "Grok"),
        Arc::new(CheckpointDesk::default()),
    )
    .expect("tools");
    let gate = Gate::new(setup.policy.clone());
    assert!(matches!(
        verdict(
            &gate,
            ActionClass::ReadLocal,
            "fs.read",
            json!({"path": "notes.md"})
        ),
        Verdict::Allow(_)
    ));
    let Verdict::Ask(write) = verdict(
        &gate,
        ActionClass::WriteLocal,
        "fs.write",
        json!({"path": "notes.md", "content": "x"}),
    ) else {
        panic!("a write asks");
    };
    assert_eq!(write.risk(), Risk::Normal);
    let Verdict::Ask(delete) = verdict(
        &gate,
        ActionClass::DeleteLocal,
        "fs.delete",
        json!({"path": "notes.md"}),
    ) else {
        panic!("a delete asks");
    };
    assert_eq!(delete.risk(), Risk::High);
}

#[tokio::test]
async fn tp_tool_13_the_tools_reach_only_groks_folder() {
    let home = Home::new();
    std::fs::write(home.0.join("secret.txt"), "the secret").expect("secret");
    let folder = agent_folder(&home.0, "Grok");
    let setup = file_tools(&folder, Arc::new(CheckpointDesk::default())).expect("tools");
    std::fs::write(folder.join("notes.md"), "buy milk").expect("notes");
    let gate = Gate::new(setup.policy.clone());
    let read = |path: &str| {
        let Verdict::Allow(token) = verdict(
            &gate,
            ActionClass::ReadLocal,
            "fs.read",
            json!({ "path": path }),
        ) else {
            panic!("reads are allowed");
        };
        token
    };
    let notes = setup.gateway.run(read("notes.md")).await.expect("inside");
    assert_eq!(notes.full, "buy milk");
    for outside in ["../../secret.txt", "~/secret.txt"] {
        assert!(
            setup.gateway.run(read(outside)).await.is_err(),
            "{outside} is refused"
        );
    }
    let absolute = home.0.join("secret.txt");
    assert!(
        setup
            .gateway
            .run(read(&absolute.display().to_string()))
            .await
            .is_err()
    );
}

#[test]
fn tp_tool_13_no_folder_means_no_tools() {
    let home = Home::new();
    let blocked = home.0.join("Corral");
    std::fs::write(&blocked, "a file where the folder should be").expect("file");
    let err = file_tools(
        &agent_folder(&home.0, "Grok"),
        Arc::new(CheckpointDesk::default()),
    )
    .err()
    .expect("refused");
    assert!(err.contains("Corral"), "says which folder: {err}");
}

#[test]
fn tp_tool_14_checkpoint_ids_are_random() {
    let id = random_id();
    assert_eq!(id.len(), 32, "128 bits");
    assert!(
        id.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "{id}"
    );
    let next = random_ids();
    let many: HashSet<String> = (0..1_000).map(|_| next()).collect();
    assert_eq!(many.len(), 1_000, "no repeats");
}
