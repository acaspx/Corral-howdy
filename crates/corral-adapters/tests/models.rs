//! The models an agent can be given (M4 item 6; TP-AED-02; security model T25). Each comes with
//! its list price, checked on the provider's pricing page, so the budget never meters a model at
//! another model's rate; a model the table doesn't have can't be chosen. The default comes first.
//! The nightly live check makes sure the provider still lists every one (EV-PROV-03).

use corral_adapters::catalog::{
    CLAUDE_MODEL, GROK_MODEL, ModelChoice, default_model, model_choice, models,
};
use corral_adapters::messages::MessagesConfig;
use corral_adapters::responses::ResponsesConfig;
use corral_adapters::smoke::check_offered_models;
use corral_core::budget::Price;
use corral_secrets::Provider;
use serde_json::json;

fn ids(provider: Provider) -> Vec<&'static str> {
    models(provider).iter().map(|m| m.id).collect()
}

#[test]
fn tp_aed_02_each_provider_offers_its_priced_models_default_first() {
    assert_eq!(ids(Provider::Xai), [GROK_MODEL]);
    assert_eq!(
        ids(Provider::Anthropic),
        [CLAUDE_MODEL, "claude-opus-5-5", "claude-haiku-5-5"]
    );
    assert!(models(Provider::OpenAi).is_empty());
    assert!(models(Provider::Meta).is_empty());
    for provider in Provider::ALL {
        assert_eq!(
            models(provider).first().map(|m| m.id),
            default_model(provider),
            "{provider:?}"
        );
    }
}

#[test]
fn tp_aed_02_each_model_carries_its_list_price() {
    let price = |provider, id| model_choice(provider, id).map(|m: &ModelChoice| m.price);
    assert_eq!(
        price(Provider::Xai, GROK_MODEL),
        Some(ResponsesConfig::xai(GROK_MODEL).price),
        "the client's own"
    );
    assert_eq!(
        price(Provider::Anthropic, CLAUDE_MODEL),
        Some(MessagesConfig::anthropic(CLAUDE_MODEL).price)
    );
    assert_eq!(
        price(Provider::Anthropic, "claude-opus-5-5"),
        Some(Price::per_million_tokens(4_000_000, 20_000_000))
    );
    assert_eq!(
        price(Provider::Anthropic, "claude-haiku-5-5"),
        Some(
            Price::per_million_tokens(100_000, 500_000)
                .with_long_prompt(100_001, 500_000, 2_500_000)
        ),
        "five times as much past 100,000 prompt tokens"
    );
    assert_eq!(
        model_choice(Provider::Anthropic, "claude-haiku-5-5").map(|m| m.name),
        Some("Claude Haiku 5.5")
    );
}

#[test]
fn tp_aed_02_a_model_the_table_doesnt_have_cant_be_chosen() {
    assert_eq!(model_choice(Provider::Xai, "grok-3"), None);
    assert_eq!(
        model_choice(Provider::Xai, CLAUDE_MODEL),
        None,
        "not another provider's"
    );
    assert_eq!(model_choice(Provider::OpenAi, "gpt-6-sol"), None);
}

#[test]
fn tp_aed_02_the_live_check_wants_every_offered_model_listed() {
    let all = json!({"data": [
        {"id": "claude-haiku-5-5"}, {"id": "claude-sonnet-5-5"}, {"id": "claude-opus-5-5"},
    ]});
    assert_eq!(check_offered_models(Provider::Anthropic, &all), Ok(()));
    let missing = json!({"data": [{"id": "claude-sonnet-5-5"}]});
    let error = check_offered_models(Provider::Anthropic, &missing).expect_err("missing");
    assert!(
        error.contains("claude-opus-5-5, claude-haiku-5-5"),
        "{error}"
    );
    assert!(check_offered_models(Provider::Anthropic, &json!({})).is_err());
    assert!(
        check_offered_models(Provider::OpenAi, &all).is_err(),
        "nothing offered yet"
    );
}
