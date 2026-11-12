//! TP-SETUP-02: checking a key is a core command, against the provider's model list (no tokens
//! spent), and the UI only sees the result. No network here: a fake probe answers.

use corral_adapters::transport::BoxFuture;
use corral_adapters::verify::{KeyCheck, KeyCheckError, ModelsProbe, check_key};
use corral_secrets::{Provider, Secret};
use std::sync::Mutex;

const KEY: &str = "sk-ant-api03-Q7v2LmN9pRt4sWx8yZa3bCd5"; // gitleaks:allow (made-up test key)

/// Each request: the URL and its headers.
type Seen = Vec<(String, Vec<(String, String)>)>;

struct FakeProbe {
    answer: Result<u16, String>,
    seen: Mutex<Seen>,
}

impl FakeProbe {
    fn answering(answer: Result<u16, String>) -> Self {
        Self {
            answer,
            seen: Mutex::default(),
        }
    }

    fn seen(&self) -> Seen {
        self.seen.lock().expect("lock").clone()
    }
}

impl ModelsProbe for FakeProbe {
    fn get<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        self.seen.lock().expect("lock").push((
            url.to_owned(),
            headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        ));
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

fn secret() -> Secret {
    Secret::new(KEY).expect("valid")
}

fn header(seen: &[(String, String)], name: &str) -> Option<String> {
    seen.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

#[tokio::test]
async fn tp_setup_02_each_provider_is_asked_for_its_model_list() {
    for (provider, url) in [
        (Provider::Xai, "https://api.x.ai/v1/models"),
        (Provider::OpenAi, "https://api.openai.com/v1/models"),
        (Provider::Anthropic, "https://api.anthropic.com/v1/models"),
    ] {
        let probe = FakeProbe::answering(Ok(200));
        assert_eq!(
            check_key(&probe, provider, &secret()).await,
            Ok(KeyCheck::Accepted)
        );
        let seen = probe.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, url);
        let headers = &seen[0].1;
        match provider {
            Provider::Anthropic => {
                assert_eq!(header(headers, "x-api-key"), Some(KEY.to_owned()));
                assert_eq!(
                    header(headers, "anthropic-version"),
                    Some("2023-06-01".into())
                );
                assert_eq!(header(headers, "authorization"), None);
            }
            Provider::Xai | Provider::OpenAi => {
                assert_eq!(
                    header(headers, "authorization"),
                    Some(format!("Bearer {KEY}"))
                );
            }
            Provider::Meta => unreachable!("not in this list"),
        }
    }
}

#[tokio::test]
async fn tp_setup_02_a_provider_without_a_known_check_is_saved_unchecked() {
    let probe = FakeProbe::answering(Ok(200));
    assert_eq!(
        check_key(&probe, Provider::Meta, &secret()).await,
        Ok(KeyCheck::NotChecked)
    );
    assert!(probe.seen().is_empty(), "nothing is sent");
}

#[tokio::test]
async fn tp_setup_02_a_refused_key_says_so_without_quoting_it() {
    for status in [401, 403] {
        let probe = FakeProbe::answering(Ok(status));
        let err = check_key(&probe, Provider::Anthropic, &secret())
            .await
            .expect_err("refused");
        assert_eq!(err, KeyCheckError::Rejected(Provider::Anthropic));
        assert_eq!(err.to_string(), "Anthropic didn't accept this key.");
    }
}

#[tokio::test]
async fn tp_setup_02_an_unreachable_provider_is_not_a_bad_key() {
    for answer in [Ok(500), Ok(429), Err("connection refused".to_owned())] {
        let probe = FakeProbe::answering(answer);
        let err = check_key(&probe, Provider::Xai, &secret())
            .await
            .expect_err("unavailable");
        assert!(
            matches!(err, KeyCheckError::Unavailable(Provider::Xai, _)),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(
            text.starts_with("Couldn't reach xAI to check the key"),
            "{text}"
        );
        assert!(!text.contains(&KEY[8..20]), "the key leaked: {text}");
    }
}
