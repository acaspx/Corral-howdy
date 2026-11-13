//! TP-LINK-02: which linked apps show is kept in Corral's own folder, written whole or not at
//! all, and a file that can't be read falls back to nothing linked rather than guessing.

use corral_adapters::linked::{DotLook, LinkedSettings};
use corral_desktop_lib::linked::{LinkedStore, LinkedStoreError, MAX_LINKED_BYTES};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "corral-linked-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear scratch");
    }
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn tp_link_02_nothing_saved_means_nothing_linked() {
    let store = LinkedStore::new(scratch().join("linked-apps.json"));
    assert_eq!(store.load(), Ok(LinkedSettings::default()));
}

#[test]
fn tp_link_02_choices_are_saved_and_read_back() {
    let dir = scratch();
    let store = LinkedStore::new(dir.join("linked-apps.json"));
    let s = LinkedSettings {
        muse: true,
        dot: Some(DotLook::Felipe),
    };
    store.save(&s).expect("saved");
    assert_eq!(store.load(), Ok(s));
    assert_eq!(names(&dir), ["linked-apps.json"], "no temp file left");
    let text = std::fs::read_to_string(dir.join("linked-apps.json")).expect("read");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).expect("json"),
        serde_json::json!({"muse": true, "dot": "felipe"})
    );
}

#[test]
fn tp_link_02_a_damaged_or_huge_file_is_an_error_not_a_guess() {
    let dir = scratch();
    let path = dir.join("linked-apps.json");
    let store = LinkedStore::new(&path);
    std::fs::write(&path, b"{\"muse\": tru").expect("write");
    assert!(
        matches!(store.load(), Err(LinkedStoreError::Malformed(_))),
        "{:?}",
        store.load()
    );
    std::fs::write(&path, vec![b' '; MAX_LINKED_BYTES + 1]).expect("write");
    assert_eq!(
        store.load(),
        Err(LinkedStoreError::TooLarge(MAX_LINKED_BYTES + 1))
    );
}
