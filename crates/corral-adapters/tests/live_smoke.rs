//! The nightly live check's judgement (M3 exit; EV-PROV-01, 03; TP-SMOKE-01..04). The check itself
//! runs against real providers from `examples/live_smoke.rs`; everything that decides pass or fail
//! is here, so it's tested offline: which keys it was given, whether a provider still lists the
//! model Corral's agent uses, and whether a run ended well with its usage reported.

use corral_adapters::catalog::{CLAUDE_MODEL, GROK_MODEL, default_model};
use corral_adapters::smoke::{
    KEY_VARS, SMOKE_PROMPT, SMOKE_RUN_CAP_MICRO_USD, check_models, judge, live_keys, models_request,
};
use corral_core::event::*;
use corral_core::ids::{RunId, SpanId};
use corral_secrets::{Provider, Secret};
use serde_json::json;
use std::collections::HashMap;

const XAI_KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)
const ANTHROPIC_KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5"; // gitleaks:allow (made-up test key)

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

// ---- TP-SMOKE-01: the keys -------------------------------------------------------------------

#[test]
fn tp_smoke_01_each_provider_with_a_key_is_checked() {
    assert_eq!(
        KEY_VARS,
        [
            (Provider::Xai, "CORRAL_LIVE_XAI_KEY"),
            (Provider::Anthropic, "CORRAL_LIVE_ANTHROPIC_KEY"),
        ]
    );
    let keys = live_keys(env(&[
        ("CORRAL_LIVE_XAI_KEY", XAI_KEY),
        ("CORRAL_LIVE_ANTHROPIC_KEY", ANTHROPIC_KEY),
    ]))
    .expect("keys");
    let providers: Vec<Provider> = keys.iter().map(|(p, _)| *p).collect();
    assert_eq!(providers, [Provider::Xai, Provider::Anthropic]);
    assert_eq!(keys[1].1.expose(), ANTHROPIC_KEY);

    // GitHub passes a secret that isn't set as an empty string.
    let only_claude = live_keys(env(&[
        ("CORRAL_LIVE_XAI_KEY", "  "),
        ("CORRAL_LIVE_ANTHROPIC_KEY", ANTHROPIC_KEY),
    ]))
    .expect("keys");
    assert_eq!(only_claude.len(), 1);
    assert_eq!(only_claude[0].0, Provider::Anthropic);
}

#[test]
fn tp_smoke_01_no_keys_fails_rather_than_passing_with_nothing_checked() {
    for given in [
        vec![],
        vec![
            ("CORRAL_LIVE_XAI_KEY", ""),
            ("CORRAL_LIVE_ANTHROPIC_KEY", ""),
        ],
    ] {
        let err = live_keys(env(&given)).expect_err("nothing to check");
        assert!(err.contains("CORRAL_LIVE_ANTHROPIC_KEY"), "{err}");
    }
}

#[test]
fn tp_smoke_01_a_bad_key_is_named_by_its_variable_never_its_value() {
    let bad = "xai-has spaces-Q7v2LmN9"; // gitleaks:allow (made-up)
    let err = live_keys(env(&[("CORRAL_LIVE_XAI_KEY", bad)])).expect_err("bad key");
    assert!(err.contains("CORRAL_LIVE_XAI_KEY"), "{err}");
    assert!(!err.contains("Q7v2"), "the key leaked: {err}");
}

// ---- TP-SMOKE-02 / EV-PROV-03: the model Corral uses is still listed ---------------------------

#[test]
fn tp_smoke_02_the_default_models_are_the_apps() {
    assert_eq!(GROK_MODEL, "grok-4.7");
    assert_eq!(CLAUDE_MODEL, "claude-sonnet-5-5");
    assert_eq!(default_model(Provider::Xai), Some(GROK_MODEL));
    assert_eq!(default_model(Provider::Anthropic), Some(CLAUDE_MODEL));
    assert_eq!(default_model(Provider::OpenAi), None);
    assert_eq!(default_model(Provider::Meta), None);
}

#[test]
fn tp_smoke_02_ev_prov_03_a_listed_model_passes_and_a_missing_one_is_named() {
    let listed =
        json!({"data": [{"id": "claude-haiku-5-5"}, {"id": CLAUDE_MODEL}], "has_more": false});
    assert_eq!(check_models(Provider::Anthropic, &listed), Ok(()));

    let gone = json!({"data": [{"id": "claude-haiku-5-5"}]});
    let err = check_models(Provider::Anthropic, &gone).expect_err("missing");
    assert!(err.contains(CLAUDE_MODEL), "{err}");
    assert!(err.contains("Anthropic"), "{err}");

    let err = check_models(Provider::Xai, &json!({"object": "list"})).expect_err("unreadable");
    assert!(err.contains("xAI"), "{err}");
}

#[test]
fn tp_smoke_02_the_models_request_is_the_key_checks() {
    let key = Secret::new(ANTHROPIC_KEY).expect("key");
    let (url, headers) = models_request(Provider::Anthropic, &key).expect("a request");
    assert_eq!(url, "https://api.anthropic.com/v1/models");
    let names: Vec<&str> = headers.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["x-api-key", "anthropic-version"]);
    let key = Secret::new(XAI_KEY).expect("key");
    let (url, _) = models_request(Provider::Xai, &key).expect("a request");
    assert_eq!(url, "https://api.x.ai/v1/models");
}

// ---- TP-SMOKE-03 / EV-PROV-01: a short task completes with its usage reported ----------------

fn event(seq: u64, body: EventBody) -> Event {
    Event {
        run_id: RunId::from("r1"),
        seq,
        ts: 1_000 + seq,
        span_id: SpanId::from("s1"),
        parent_span_id: None,
        body,
    }
}

fn answered(text: &str) -> EventBody {
    EventBody::ModelResponse(ModelResponse {
        text: text.into(),
        finish_reason: Some("end_turn".into()),
    })
}

fn usage(input: u64, output: u64, cost: u64) -> EventBody {
    EventBody::UsageReported(UsageReported {
        input_tokens: input,
        output_tokens: output,
        cost_micro_usd: cost,
    })
}

fn completed() -> EventBody {
    EventBody::RunCompleted(RunCompleted { summary: None })
}

fn run(bodies: Vec<EventBody>) -> Vec<Event> {
    bodies
        .into_iter()
        .enumerate()
        .map(|(n, body)| event(n as u64 + 1, body))
        .collect()
}

#[test]
fn tp_smoke_03_ev_prov_01_a_completed_run_with_usage_passes() {
    let events = run(vec![
        answered("Corral is ready now."),
        usage(20, 6, 100),
        completed(),
    ]);
    let outcome = judge(&events).expect("passes");
    assert_eq!(outcome.input_tokens, 20);
    assert_eq!(outcome.output_tokens, 6);
    assert_eq!(outcome.cost_micro_usd, 100);
    assert_eq!(outcome.answer, "Corral is ready now.");

    // Usage across turns adds up.
    let two_turns = run(vec![
        usage(10, 2, 40),
        answered("ok"),
        usage(15, 3, 60),
        completed(),
    ]);
    let outcome = judge(&two_turns).expect("passes");
    assert_eq!(
        (
            outcome.input_tokens,
            outcome.output_tokens,
            outcome.cost_micro_usd
        ),
        (25, 5, 100)
    );
}

#[test]
fn tp_smoke_03_a_run_that_didnt_end_well_fails_with_its_message() {
    let failed = run(vec![EventBody::RunFailed(RunFailed {
        reason: FailureReason::Provider,
        message: "Anthropic is overloaded.".into(),
    })]);
    let err = judge(&failed).expect_err("failed");
    assert!(err.contains("Anthropic is overloaded."), "{err}");

    let cancelled = run(vec![EventBody::RunCancelled(RunCancelled {
        by: CancelledBy::User,
    })]);
    assert!(judge(&cancelled).is_err());
    assert!(judge(&[]).expect_err("nothing").contains("didn't end"));
    assert!(
        judge(&run(vec![answered("hi"), usage(1, 1, 1)])).is_err(),
        "no terminal event"
    );
}

#[test]
fn tp_smoke_03_missing_usage_or_an_empty_answer_fails() {
    for (bodies, why) in [
        (vec![answered("hi"), completed()], "usage"),
        (
            vec![answered("hi"), usage(0, 4, 10), completed()],
            "input tokens",
        ),
        (
            vec![answered("hi"), usage(4, 0, 10), completed()],
            "output tokens",
        ),
        (vec![answered("hi"), usage(4, 4, 0), completed()], "cost"),
        (vec![answered("  "), usage(4, 4, 10), completed()], "answer"),
        (vec![usage(4, 4, 10), completed()], "answer"),
    ] {
        let err = judge(&run(bodies)).expect_err(why);
        assert!(err.contains(why), "{why}: {err}");
    }
}

#[test]
fn tp_smoke_03_events_after_the_end_fail() {
    let events = run(vec![
        answered("hi"),
        usage(4, 4, 10),
        completed(),
        completed(),
    ]);
    assert!(judge(&events).expect_err("two ends").contains("after"));
}

// ---- TP-SMOKE-04: the task and its cap ------------------------------------------------------

#[test]
fn tp_smoke_04_one_short_benign_task_under_a_five_cent_cap() {
    assert_eq!(
        SMOKE_PROMPT,
        "Reply with exactly these four words: Corral is ready now."
    );
    assert_eq!(SMOKE_RUN_CAP_MICRO_USD, 50_000);
}
