//! TP-LOGO-08: provider logos are kept in Corral's own folder under fixed names, written whole or
//! not at all, and fetched only when missing (ADR-015). No network: a fake fetcher answers.

use corral_adapters::logos::{
    Logo, LogoError, LogoFetcher, LogoFormat, LogoId, LogoSource, logo_source,
};
use corral_adapters::transport::BoxFuture;
use corral_desktop_lib::logos::{LogoStore, LogoStoreError, fetch_missing};
use corral_secrets::Provider;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

fn png(side: u32) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(&13_u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&side.to_be_bytes());
    b.extend_from_slice(&side.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b.extend_from_slice(&[0; 32]);
    b
}

fn ico() -> Vec<u8> {
    let mut b = vec![0, 0, 1, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0];
    b.extend_from_slice(&64_u32.to_le_bytes());
    b.extend_from_slice(&22_u32.to_le_bytes());
    b.extend_from_slice(&[0; 64]);
    b
}

/// A fresh, empty folder for one test.
fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "corral-logos-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear scratch");
    }
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable();
    names
}

fn logo(provider: Provider, format: LogoFormat, bytes: Vec<u8>) -> Logo {
    Logo {
        id: LogoId::Provider(provider),
        format,
        bytes,
    }
}

const CLAUDE: LogoId = LogoId::Provider(Provider::Anthropic);
const GROK: LogoId = LogoId::Provider(Provider::Xai);

struct FakeFetcher {
    answer: Result<Vec<u8>, LogoError>,
    seen: Mutex<Vec<String>>,
}

impl LogoFetcher for FakeFetcher {
    fn get<'a>(&'a self, source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>> {
        self.seen.lock().expect("lock").push(source.url.to_owned());
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

fn fetcher(answer: Result<Vec<u8>, LogoError>) -> FakeFetcher {
    FakeFetcher {
        answer,
        seen: Mutex::default(),
    }
}

#[test]
fn tp_logo_08_a_logo_is_saved_under_a_fixed_name_in_corrals_folder() {
    let dir = scratch();
    let store = LogoStore::new(dir.join("logos"));
    let saved = store
        .save(&logo(Provider::Anthropic, LogoFormat::Png, png(180)))
        .expect("saved");
    assert_eq!(saved, dir.join("logos").join("anthropic.png"));
    assert_eq!(std::fs::read(&saved).expect("read"), png(180));
    assert_eq!(store.path(Provider::Anthropic), Some(saved));
    assert_eq!(
        names(&dir.join("logos")),
        ["anthropic.png"],
        "no temp files left"
    );

    let ico_path = store
        .save(&logo(Provider::Xai, LogoFormat::Ico, ico()))
        .expect("saved");
    assert_eq!(ico_path, dir.join("logos").join("xai.ico"));
    assert_eq!(store.path(Provider::Xai), Some(ico_path));
    assert_eq!(store.path(Provider::OpenAi), None);
}

#[test]
fn tp_logo_08_bytes_that_fail_the_check_are_never_written() {
    let dir = scratch();
    let store = LogoStore::new(dir.join("logos"));
    let refused = store.save(&logo(
        Provider::Anthropic,
        LogoFormat::Png,
        b"<html>".to_vec(),
    ));
    assert_eq!(refused, Err(LogoStoreError::Fetch(LogoError::NotAnImage)));
    // A format that doesn't match the bytes is refused too: the name says what's inside.
    let mismatched = store.save(&logo(Provider::Anthropic, LogoFormat::Ico, png(180)));
    assert!(mismatched.is_err(), "{mismatched:?}");
    assert!(names(&dir.join("logos")).is_empty());
    assert_eq!(store.path(Provider::Anthropic), None);
}

#[test]
fn tp_logo_08_a_new_logo_replaces_the_old_whatever_its_format() {
    let dir = scratch();
    let store = LogoStore::new(dir.join("logos"));
    store
        .save(&logo(Provider::Xai, LogoFormat::Ico, ico()))
        .expect("ico");
    let png_path = store
        .save(&logo(Provider::Xai, LogoFormat::Png, png(64)))
        .expect("png");
    assert_eq!(names(&dir.join("logos")), ["xai.png"]);
    assert_eq!(store.path(Provider::Xai), Some(png_path));
}

#[tokio::test]
async fn tp_logo_08_only_missing_logos_are_fetched() {
    let dir = scratch();
    let store = LogoStore::new(dir.join("logos"));
    let kept = store
        .save(&logo(Provider::Anthropic, LogoFormat::Png, png(180)))
        .expect("saved");
    let fake = fetcher(Ok(png(120)));
    let results = fetch_missing(&fake, &store, &[CLAUDE, GROK]).await;

    let xai = logo_source(Provider::Xai).expect("Grok has a logo");
    assert_eq!(fake.seen(), [xai.url], "Claude's logo was already there");
    assert_eq!(
        results,
        [
            (CLAUDE, Ok(kept)),
            (GROK, Ok(dir.join("logos").join("xai.png"))),
        ]
    );
}

#[tokio::test]
async fn tp_logo_08_a_failed_fetch_is_reported_and_stores_nothing() {
    let dir = scratch();
    let store = LogoStore::new(dir.join("logos"));
    let fake = fetcher(Err(LogoError::Http(404)));
    let results = fetch_missing(&fake, &store, &[GROK]).await;
    assert_eq!(
        results,
        [(GROK, Err(LogoStoreError::Fetch(LogoError::Http(404))))]
    );
    assert_eq!(store.path(Provider::Xai), None);
}

impl FakeFetcher {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}
