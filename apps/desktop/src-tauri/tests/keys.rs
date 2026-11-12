//! The provider key commands: TP-SETUP-01 and TP-SEC-02 at the IPC boundary (`docs/04-test-plan.md`,
//! security model T6). The webview sends a key in and only ever gets a status back.

use corral_desktop_lib::keys::{key_status, remove_key, save_key};
use corral_secrets::{MemoryStore, Provider};

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

fn assert_no_key(text: &str) {
    for i in 0..=KEY.len() - 5 {
        let f = &KEY[i..i + 5];
        assert!(
            !text.contains(f),
            "`{text}` contains part of the key (`{f}`)"
        );
    }
}

#[test]
fn tp_setup_01_saving_returns_a_status_never_the_key() {
    let store = MemoryStore::default();
    let saved = save_key(&store, Provider::Xai, KEY.to_owned()).unwrap();
    let json = serde_json::to_value(&saved).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "provider": "xai", "configured": true, "last4": "hJk1" })
    );
    assert_no_key(&json.to_string().replace("hJk1", ""));
    assert_eq!(key_status(&store, Provider::Xai).unwrap(), saved);
}

#[test]
fn tp_setup_01_a_bad_key_is_refused_without_echoing_it_and_nothing_is_stored() {
    let store = MemoryStore::default();
    let err = save_key(&store, Provider::Xai, format!("{KEY} {KEY}")).unwrap_err();
    assert_no_key(&err);
    assert!(!key_status(&store, Provider::Xai).unwrap().configured);
}

#[test]
fn tp_sec_02_removing_a_key_leaves_nothing_to_show() {
    let store = MemoryStore::default();
    save_key(&store, Provider::Xai, KEY.to_owned()).unwrap();
    let removed = remove_key(&store, Provider::Xai).unwrap();
    assert!(!removed.configured);
    assert_eq!(removed.last4, None);
}
