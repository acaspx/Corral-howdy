//! Connecting a key from the single key field: TP-SETUP-02 and TP-SETUP-05 at the IPC boundary.
//! The core checks the key with its provider before storing it; a refused key is never stored.

use corral_adapters::transport::BoxFuture;
use corral_adapters::verify::ModelsProbe;
use corral_desktop_lib::keys::{Connected, connect_key, identify_key};
use corral_secrets::{KeyStatus, MemoryStore, Provider, SecretStore};

const KEY: &str = "xai-Q7v2LmN9pRt4sWx8yZa3bCd5eFg6hJk1"; // gitleaks:allow (made-up test key)

struct Answer(Result<u16, String>);

impl ModelsProbe for Answer {
    fn get<'a>(
        &'a self,
        _url: &'a str,
        _headers: &'a [(&'static str, String)],
    ) -> BoxFuture<'a, Result<u16, String>> {
        let answer = self.0.clone();
        Box::pin(async move { answer })
    }
}

fn assert_no_key(text: &str) {
    for i in 0..=KEY.len() - 5 {
        let f = &KEY[i..i + 5];
        assert!(!text.contains(f), "`{text}` contains part of the key");
    }
}

#[test]
fn tp_setup_05_the_field_suggests_a_provider() {
    assert_eq!(identify_key(KEY), Some(Provider::Xai));
    assert_eq!(identify_key("something-else"), None);
}

#[tokio::test]
async fn tp_setup_02_an_accepted_key_is_stored_and_reported_without_the_key() {
    let store = MemoryStore::default();
    let connected = connect_key(&store, &Answer(Ok(200)), Provider::Xai, KEY.to_owned())
        .await
        .expect("connected");
    assert_eq!(
        connected,
        Connected {
            status: KeyStatus {
                provider: Provider::Xai,
                configured: true,
                last4: Some("hJk1".into()),
            },
            checked: true,
        }
    );
    let json = serde_json::to_value(&connected).expect("json");
    assert_eq!(json["checked"], true);
    assert_no_key(&json.to_string().replace("hJk1", ""));
    assert!(store.get(Provider::Xai).expect("read").is_some());
}

#[tokio::test]
async fn tp_setup_02_a_refused_key_is_not_stored() {
    let store = MemoryStore::default();
    let err = connect_key(&store, &Answer(Ok(401)), Provider::Xai, KEY.to_owned())
        .await
        .expect_err("refused");
    assert_eq!(err, "xAI didn't accept this key.");
    assert!(store.get(Provider::Xai).expect("read").is_none());
}

#[tokio::test]
async fn tp_setup_02_an_unchecked_key_is_not_stored_while_the_provider_is_down() {
    let store = MemoryStore::default();
    let err = connect_key(
        &store,
        &Answer(Err("timed out".into())),
        Provider::Xai,
        KEY.to_owned(),
    )
    .await
    .expect_err("unavailable");
    assert!(err.starts_with("Couldn't reach xAI"), "{err}");
    assert_no_key(&err);
    assert!(store.get(Provider::Xai).expect("read").is_none());
}

#[tokio::test]
async fn tp_setup_01_a_malformed_key_is_refused_before_anything_is_sent() {
    let store = MemoryStore::default();
    let err = connect_key(
        &store,
        &Answer(Ok(200)),
        Provider::Xai,
        "has spaces in it".to_owned(),
    )
    .await
    .expect_err("malformed");
    assert_no_key(&err);
    assert!(store.get(Provider::Xai).expect("read").is_none());
}

#[tokio::test]
async fn tp_setup_02_a_provider_without_a_check_is_stored_and_marked_unchecked() {
    let store = MemoryStore::default();
    let connected = connect_key(&store, &Answer(Ok(500)), Provider::Meta, KEY.to_owned())
        .await
        .expect("stored");
    assert!(!connected.checked);
    assert!(connected.status.configured);
}
