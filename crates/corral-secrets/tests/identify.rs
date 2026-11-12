//! TP-SETUP-05: one key field for every provider. The core suggests the provider from the key's
//! prefix; the user can correct it, and the provider's own check (TP-SETUP-02) has the last word.

use corral_secrets::{Provider, identify};

#[test]
fn tp_setup_05_a_key_names_its_provider_by_prefix() {
    assert_eq!(identify("xai-a1b2c3d4"), Some(Provider::Xai));
    assert_eq!(identify("sk-ant-api03-a1b2c3d4"), Some(Provider::Anthropic));
    assert_eq!(identify("sk-proj-a1b2c3d4"), Some(Provider::OpenAi));
    assert_eq!(identify("sk-svcacct-a1b2c3d4"), Some(Provider::OpenAi));
    assert_eq!(
        identify("sk-a1b2c3d4"),
        Some(Provider::OpenAi),
        "older OpenAI keys"
    );
}

#[test]
fn tp_setup_05_surrounding_whitespace_is_ignored() {
    assert_eq!(identify("  xai-a1b2c3d4\n"), Some(Provider::Xai));
}

#[test]
fn tp_setup_05_an_unrecognized_key_asks_instead_of_guessing() {
    for key in [
        "",
        "xai-",
        "sk-",
        "sk-ant-",
        "XAI-a1b2c3d4",
        "a1b2c3d4e5f6",
        "Bearer xai-a1b2c3d4",
    ] {
        assert_eq!(identify(key), None, "{key:?}");
    }
}
