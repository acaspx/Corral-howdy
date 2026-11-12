//! Provider keys: TP-SEC-02, TP-SEC-03 and the key half of TP-SKEL-01 (`docs/04-test-plan.md`,
//! docs/01 §11, security model T6).

use corral_secrets::{
    KeyError, MemoryStore, Provider, Secret, SecretError, SecretStore, resolve, status,
};

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

/// Every 5-character slice of `key`: none may appear anywhere the key mustn't be.
fn fragments(key: &str) -> Vec<&str> {
    (0..=key.len() - 5).map(|i| &key[i..i + 5]).collect()
}

fn assert_no_key(text: &str, key: &str) {
    for f in fragments(key) {
        assert!(
            !text.contains(f),
            "`{text}` contains part of the key (`{f}`)"
        );
    }
}

fn store_with(key: &str) -> MemoryStore {
    let store = MemoryStore::default();
    store
        .set(Provider::Xai, &Secret::new(key).expect("a valid key"))
        .expect("the memory store accepts it");
    store
}

// ---- TP-SEC-02: only the last 4 characters ever come back -----------------------------------

#[test]
fn tp_sec_02_status_shows_only_the_last_four_characters() {
    let store = store_with(KEY);
    let s = status(&store, Provider::Xai).unwrap();
    assert!(s.configured);
    assert_eq!(s.last4.as_deref(), Some("hJk1"));
    let json = serde_json::to_string(&s).unwrap();
    assert_no_key(&json, &KEY[..KEY.len() - 4]);
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        serde_json::json!({ "provider": "xai", "configured": true, "last4": "hJk1" })
    );
}

#[test]
fn tp_sec_02_status_without_a_key() {
    let s = status(&MemoryStore::default(), Provider::Xai).unwrap();
    assert!(!s.configured);
    assert_eq!(s.last4, None);
}

// ---- TP-SEC-03: validation and redaction -----------------------------------------------------

#[test]
fn tp_sec_03_debug_and_errors_never_print_the_key() {
    let secret = Secret::new(KEY).unwrap();
    assert_no_key(&format!("{secret:?}"), KEY);
    let s = status(&store_with(KEY), Provider::Xai).unwrap();
    assert_no_key(&format!("{s:?}"), &KEY[..KEY.len() - 4]);
    let bad = format!("{KEY} {KEY}");
    let err = Secret::new(bad.clone()).unwrap_err();
    assert_no_key(&err.to_string(), KEY);
    assert_no_key(&format!("{err:?}"), KEY);
}

#[test]
fn tp_sec_03_surrounding_whitespace_from_a_paste_is_trimmed() {
    let secret = Secret::new(format!("  {KEY}\n")).unwrap();
    assert_eq!(secret.expose(), KEY);
}

#[test]
fn tp_sec_03_malformed_keys_are_refused() {
    let too_long = "k".repeat(513);
    let cases: [(&str, SecretError); 6] = [
        ("", SecretError::Empty),
        ("   \n", SecretError::Empty),
        ("short", SecretError::TooShort),
        (too_long.as_str(), SecretError::TooLong),
        ("xai-abc def12345", SecretError::InvalidCharacters),
        ("xai-abcdéf12345", SecretError::InvalidCharacters),
    ];
    for (raw, expected) in cases {
        assert_eq!(Secret::new(raw).unwrap_err(), expected, "for {raw:?}");
    }
    assert_eq!(
        Secret::new("xai-abc\ndef12345").unwrap_err(),
        SecretError::InvalidCharacters
    );
}

#[test]
fn tp_sec_03_every_provider_has_a_stable_id_and_a_name() {
    let ids: Vec<&str> = Provider::ALL.iter().map(|p| p.id()).collect();
    assert_eq!(ids, ["xai", "openai", "anthropic", "meta"]);
    assert_eq!(Provider::Xai.display_name(), "xAI");
    for p in Provider::ALL {
        let json = serde_json::to_value(p).unwrap();
        assert_eq!(json, p.id());
        assert_eq!(serde_json::from_value::<Provider>(json).unwrap(), p);
    }
    assert!(serde_json::from_str::<Provider>("\"grok\"").is_err());
}

// ---- TP-SKEL-01 (key half): read at call time; missing means "Add your xAI key" --------------

#[test]
fn tp_skel_01_with_no_key_the_answer_is_add_your_xai_key() {
    let err = resolve(&MemoryStore::default(), Provider::Xai).unwrap_err();
    assert!(matches!(err, KeyError::Missing(Provider::Xai)));
    assert_eq!(err.to_string(), "Add your xAI key");
}

#[test]
fn tp_skel_01_the_key_is_read_at_call_time() {
    let store = store_with(KEY);
    assert_eq!(resolve(&store, Provider::Xai).unwrap().expose(), KEY);

    let rotated = "xai-Z9y8X7w6V5u4T3s2R1q0"; // gitleaks:allow (made-up test key)
    store
        .set(Provider::Xai, &Secret::new(rotated).unwrap())
        .unwrap();
    assert_eq!(resolve(&store, Provider::Xai).unwrap().expose(), rotated);

    store.delete(Provider::Xai).unwrap();
    assert!(matches!(
        resolve(&store, Provider::Xai),
        Err(KeyError::Missing(Provider::Xai))
    ));
}

#[test]
fn removing_a_key_that_isnt_there_is_fine() {
    MemoryStore::default().delete(Provider::Xai).unwrap();
}

#[test]
fn keys_are_per_provider() {
    let store = store_with(KEY);
    assert!(matches!(
        resolve(&store, Provider::OpenAi),
        Err(KeyError::Missing(Provider::OpenAi))
    ));
}

// ---- The real Keychain (macOS only; run by hand, never in CI) --------------------------------

/// Uses its own service prefix, so it never touches Corral's real items.
/// `cargo test -p corral-secrets -- --ignored keychain`
#[cfg(target_os = "macos")]
#[test]
#[ignore = "touches the login Keychain; run by hand on a Mac"]
fn keychain_round_trip() {
    use corral_secrets::KeychainStore;
    let store = KeychainStore::with_prefix(format!("corral-test-{}", std::process::id()));
    assert!(store.get(Provider::Xai).unwrap().is_none());
    store
        .set(Provider::Xai, &Secret::new(KEY).unwrap())
        .unwrap();
    assert_eq!(store.get(Provider::Xai).unwrap().unwrap().expose(), KEY);
    assert_eq!(
        status(&store, Provider::Xai).unwrap().last4.as_deref(),
        Some("hJk1")
    );
    store.delete(Provider::Xai).unwrap();
    assert!(store.get(Provider::Xai).unwrap().is_none());
    store.delete(Provider::Xai).unwrap();
}
