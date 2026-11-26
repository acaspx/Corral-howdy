//! A ChatGPT plan's tokens (ADR-017): the store half of TP-SUB-01 and TP-SUB-02
//! (`docs/04-test-plan.md`). The tokens go only to the plan's own item, never print, and a
//! stored record that isn't one is refused.

use corral_secrets::{
    MemoryPlanStore, PLAN_ACCOUNT, PLAN_ITEM, PlanCredentials, PlanStore, Secret, SecretError,
};

const ACCESS: &str = "plan-access-Zq81mXv3TtYb7cLp0wRn"; // gitleaks:allow (made-up test token)
const REFRESH: &str = "plan-refresh-Ux5bWk0aGm7rCz2fYq9"; // gitleaks:allow (made-up test token)
const ID: &str = "id-token-header.id-token-claims.id-token-signature"; // gitleaks:allow (made-up)

fn credentials() -> PlanCredentials {
    PlanCredentials {
        email: "anton@example.test".into(),
        subject: "user-test-subject".into(),
        client_id: "oaiapp_corral_test".into(),
        host_id: "urn:uuid:3f2b8c1e-9d4a-4e7b-8a6f-1c2d3e4f5a6b".into(),
        id_token: Secret::token(ID).expect("token"),
        access_token: Secret::token(ACCESS).expect("token"),
        refresh_token: Secret::token(REFRESH).expect("token"),
        expires_at_ms: 1_790_004_600_000,
        scopes: vec!["openid".into(), "chatgpt.tokens.use.direct".into()],
        saved_at_ms: 1_790_001_000_000,
    }
}

#[test]
fn tp_sub_01_the_plan_has_its_own_keychain_item() {
    assert_eq!(PLAN_ITEM, "openai-plan", "the item is corral.openai-plan");
    assert_eq!(PLAN_ACCOUNT, "oauth");
}

#[test]
fn tp_sub_02_tokens_are_longer_than_keys_but_still_bounded() {
    let jwt = format!("eyJ{}", "a".repeat(4_000));
    assert!(
        Secret::new(jwt.clone()).is_err(),
        "an API key is at most 512 characters"
    );
    assert_eq!(Secret::token(jwt.clone()).unwrap().expose(), jwt);
    assert!(matches!(
        Secret::token("a".repeat(Secret::MAX_TOKEN_LEN + 1)),
        Err(SecretError::TooLong)
    ));
    assert!(matches!(Secret::token(""), Err(SecretError::Empty)));
    assert!(matches!(
        Secret::token("abc def"),
        Err(SecretError::InvalidCharacters)
    ));
    assert_eq!(
        format!("{:?}", Secret::token(ACCESS).unwrap()),
        "Secret(redacted)"
    );
}

#[test]
fn tp_sub_02_the_memory_store_keeps_one_plan_and_forgets_it() {
    let store = MemoryPlanStore::default();
    assert!(store.get_plan().unwrap().is_none());
    store.set_plan(&credentials()).unwrap();
    let back = store.get_plan().unwrap().unwrap();
    assert_eq!(back.access_token.expose(), ACCESS);
    assert_eq!(back.refresh_token.expose(), REFRESH);
    assert_eq!(back.id_token.expose(), ID);
    assert_eq!(back.email, "anton@example.test");
    store.delete_plan().unwrap();
    assert!(store.get_plan().unwrap().is_none());
    store.delete_plan().unwrap();
}

#[test]
fn tp_sub_02_a_record_is_json_with_every_field_and_a_bad_one_is_refused() {
    let record = credentials().to_record();
    let value: serde_json::Value = serde_json::from_str(&record).unwrap();
    for field in [
        "email",
        "subject",
        "client_id",
        "host_id",
        "id_token",
        "access_token",
        "refresh_token",
        "expires_at_ms",
        "scopes",
        "saved_at_ms",
    ] {
        assert!(value.get(field).is_some(), "{field} is kept");
    }
    for bad in [
        "",
        "not json",
        r#"{"email":"a"}"#,
        &record.replace(ACCESS, "has a space"),
    ] {
        assert!(
            matches!(
                PlanCredentials::from_record(bad),
                Err(SecretError::Unreadable)
            ),
            "{bad:?} must be refused"
        );
    }
}

/// `cargo test -p corral-secrets -- --ignored keychain`
#[cfg(target_os = "macos")]
#[test]
#[ignore = "touches the login Keychain; run by hand on a Mac"]
fn keychain_plan_round_trip() {
    use corral_secrets::KeychainStore;
    let store = KeychainStore::with_prefix(format!("corral-test-{}", std::process::id()));
    assert!(store.get_plan().unwrap().is_none());
    store.set_plan(&credentials()).unwrap();
    assert_eq!(
        store.get_plan().unwrap().unwrap().access_token.expose(),
        ACCESS
    );
    store.delete_plan().unwrap();
    assert!(store.get_plan().unwrap().is_none());
}
