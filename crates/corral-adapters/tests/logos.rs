//! TP-LOGO-05..07: provider logos, fetched at runtime (ADR-015). Each provider's logo comes from
//! one fixed HTTPS URL on its own site, is size- and pixel-capped before anything stores it, and
//! is never sent the key. No network here: a fake fetcher answers.

use corral_adapters::logos::{
    Logo, LogoError, LogoFetcher, LogoFormat, LogoId, LogoSource, MAX_LOGO_BYTES, MAX_LOGO_PIXELS,
    check_logo, fetch_logo, logo_source, read_capped, redirect_allowed,
};
use corral_adapters::transport::BoxFuture;
use corral_secrets::Provider;
use std::sync::Mutex;

/// A PNG's first bytes: the signature and an IHDR chunk for `width` × `height`. Enough for the
/// header check; decoding is ImageIO's job, in Swift.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(&13_u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&width.to_be_bytes());
    b.extend_from_slice(&height.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 0]); // CRC, not checked here
    b.extend_from_slice(&[0; 64]);
    b
}

/// An ICO with one 32 × 32 image entry.
fn ico() -> Vec<u8> {
    let mut b = vec![0, 0, 1, 0, 1, 0];
    b.extend_from_slice(&[32, 32, 0, 0, 1, 0, 32, 0]);
    b.extend_from_slice(&64_u32.to_le_bytes());
    b.extend_from_slice(&22_u32.to_le_bytes());
    b.extend_from_slice(&[0; 64]);
    b
}

/// The provider's own domains: a logo may come from nowhere else.
fn own_domains(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Xai => &["grok.com", "x.ai"],
        Provider::Anthropic => &["claude.ai", "anthropic.com"],
        Provider::OpenAi => &["chatgpt.com", "openai.com"],
        Provider::Meta => &["meta.ai", "meta.com"],
    }
}

fn on_domain(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn host_of(url: &str) -> &str {
    let rest = url.strip_prefix("https://").expect("https");
    rest.split(['/', ':', '?', '#']).next().expect("a host")
}

struct FakeFetcher {
    answer: Result<Vec<u8>, LogoError>,
    seen: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn answering(answer: Result<Vec<u8>, LogoError>) -> Self {
        Self {
            answer,
            seen: Mutex::default(),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}

impl LogoFetcher for FakeFetcher {
    fn get<'a>(&'a self, source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>> {
        self.seen.lock().expect("lock").push(source.url.to_owned());
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

// ---- TP-LOGO-05: one fixed HTTPS URL per provider, on its own site ----------------------------

#[test]
fn tp_logo_05_every_logo_comes_from_the_providers_own_site_over_https() {
    for provider in Provider::ALL {
        let Some(source) = logo_source(provider) else {
            continue;
        };
        assert!(source.url.starts_with("https://"), "{}", source.url);
        let host = host_of(source.url);
        assert!(
            source.hosts.contains(&host),
            "{provider:?}: the URL's host is allowed"
        );
        assert!(
            redirect_allowed(source, source.url),
            "{provider:?}: the URL itself passes the redirect rule"
        );
        for allowed in source.hosts {
            assert!(
                own_domains(provider).iter().any(|d| on_domain(allowed, d)),
                "{provider:?}: {allowed} isn't the provider's own"
            );
        }
    }
    assert!(
        logo_source(Provider::Xai).is_some() && logo_source(Provider::Anthropic).is_some(),
        "Grok and Claude, the agents that run today, have logos"
    );
}

#[test]
fn tp_logo_05_redirects_stay_on_https_and_the_providers_hosts() {
    let source = LogoSource {
        url: "https://claude.ai/apple-touch-icon.png",
        hosts: &["claude.ai", "www.claude.ai"],
        path_prefix: "/",
    };
    assert!(redirect_allowed(&source, "https://www.claude.ai/icon.png"));
    assert!(redirect_allowed(&source, "https://claude.ai/other.png"));
    assert!(
        !redirect_allowed(&source, "http://claude.ai/icon.png"),
        "no HTTP"
    );
    assert!(!redirect_allowed(&source, "https://evil.example/icon.png"));
    assert!(
        !redirect_allowed(&source, "https://claude.ai.evil.example/icon.png"),
        "a lookalike suffix"
    );
    assert!(
        !redirect_allowed(&source, "https://user@evil.example/claude.ai"),
        "userinfo tricks"
    );
    assert!(!redirect_allowed(&source, "not a url"));

    // A shared image host is pinned to the owner's folder on it.
    let pinned = LogoSource {
        url: "https://images.example-cdn.net/owner-space/logo.png",
        hosts: &["images.example-cdn.net"],
        path_prefix: "/owner-space/",
    };
    assert!(redirect_allowed(
        &pinned,
        "https://images.example-cdn.net/owner-space/other.png"
    ));
    assert!(
        !redirect_allowed(
            &pinned,
            "https://images.example-cdn.net/someone-else/logo.png"
        ),
        "another tenant on the same host"
    );
    assert!(
        !redirect_allowed(
            &pinned,
            "https://images.example-cdn.net/owner-space/../someone-else/logo.png"
        ),
        "dot segments are resolved before the check"
    );
}

#[test]
fn logo_ids_name_their_files() {
    assert_eq!(LogoId::from(Provider::Anthropic).name(), "anthropic");
    assert_eq!(LogoId::from(Provider::Xai).name(), "xai");
}

// ---- TP-LOGO-06: size, type and pixel caps before anything is stored ---------------------------

#[test]
fn tp_logo_06_png_and_ico_within_the_caps_are_accepted() {
    assert_eq!(check_logo(&png(180, 180)), Ok(LogoFormat::Png));
    assert_eq!(
        check_logo(&png(MAX_LOGO_PIXELS, MAX_LOGO_PIXELS)),
        Ok(LogoFormat::Png)
    );
    assert_eq!(check_logo(&ico()), Ok(LogoFormat::Ico));
}

#[test]
fn tp_logo_06_too_large_too_many_pixels_and_not_images_are_refused() {
    let mut big = png(64, 64);
    big.resize(MAX_LOGO_BYTES + 1, 0);
    assert_eq!(
        check_logo(&big),
        Err(LogoError::TooLarge(MAX_LOGO_BYTES + 1))
    );
    assert_eq!(
        check_logo(&png(MAX_LOGO_PIXELS + 1, 64)),
        Err(LogoError::TooManyPixels {
            width: MAX_LOGO_PIXELS + 1,
            height: 64
        })
    );
    assert_eq!(
        check_logo(&png(0, 64)),
        Err(LogoError::TooManyPixels {
            width: 0,
            height: 64
        }),
        "an empty image"
    );
    for not_an_image in [
        b"<!doctype html><html>sign in</html>".to_vec(),
        b"<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>".to_vec(),
        Vec::new(),
        png(180, 180)[..20].to_vec(),
        vec![0, 0, 1, 0, 0, 0],
    ] {
        assert_eq!(
            check_logo(&not_an_image),
            Err(LogoError::NotAnImage),
            "{not_an_image:?}"
        );
    }
}

#[tokio::test]
async fn tp_logo_06_the_download_stops_at_the_cap() {
    let chunks = vec![
        Ok::<_, std::io::Error>(vec![0_u8; MAX_LOGO_BYTES / 2]),
        Ok(vec![0_u8; MAX_LOGO_BYTES / 2]),
        Ok(vec![0_u8; 1]),
        Ok(vec![0_u8; MAX_LOGO_BYTES]),
    ];
    let read = read_capped(futures_util::stream::iter(chunks), MAX_LOGO_BYTES).await;
    assert_eq!(read, Err(LogoError::TooLarge(MAX_LOGO_BYTES + 1)));

    let fits = vec![Ok::<_, std::io::Error>(b"ab".to_vec()), Ok(b"cd".to_vec())];
    let read = read_capped(futures_util::stream::iter(fits), 4).await;
    assert_eq!(read, Ok(b"abcd".to_vec()));

    let broken = vec![
        Ok(b"ab".to_vec()),
        Err(std::io::Error::other("connection reset")),
    ];
    let read = read_capped(futures_util::stream::iter(broken), 4).await;
    assert!(matches!(read, Err(LogoError::Fetch(_))), "{read:?}");
}

// ---- TP-LOGO-07: fetch_logo -------------------------------------------------------------------

#[tokio::test]
async fn tp_logo_07_a_good_logo_comes_back_checked() {
    let fetcher = FakeFetcher::answering(Ok(png(180, 180)));
    let logo = fetch_logo(&fetcher, Provider::Anthropic)
        .await
        .expect("a logo");
    assert_eq!(
        logo,
        Logo {
            id: LogoId::Provider(Provider::Anthropic),
            format: LogoFormat::Png,
            bytes: png(180, 180),
        }
    );
    let source = logo_source(Provider::Anthropic).expect("Claude has a logo");
    assert_eq!(
        fetcher.seen(),
        [source.url],
        "only the fixed URL, fetched once"
    );
}

#[tokio::test]
async fn tp_logo_07_a_bad_answer_is_an_error_and_not_a_logo() {
    for (answer, expected) in [
        (Ok(b"<html>".to_vec()), LogoError::NotAnImage),
        (Err(LogoError::Http(404)), LogoError::Http(404)),
        (Err(LogoError::Redirect), LogoError::Redirect),
        (
            Err(LogoError::Fetch("timed out".into())),
            LogoError::Fetch("timed out".into()),
        ),
    ] {
        let fetcher = FakeFetcher::answering(answer);
        assert_eq!(
            fetch_logo(&fetcher, Provider::Xai).await,
            Err(expected.clone()),
            "{expected:?}"
        );
    }
}

#[tokio::test]
async fn tp_logo_07_a_provider_without_a_source_isnt_fetched() {
    let fetcher = FakeFetcher::answering(Ok(png(180, 180)));
    for provider in Provider::ALL {
        if logo_source(provider).is_none() {
            assert_eq!(
                fetch_logo(&fetcher, provider).await,
                Err(LogoError::NoSource)
            );
        }
    }
    assert!(fetcher.seen().is_empty(), "nothing was fetched");
}

#[test]
fn logo_errors_read_plainly() {
    assert_eq!(
        LogoError::TooLarge(2_000_000).to_string(),
        "the logo is larger than 1 MB"
    );
    assert_eq!(
        LogoError::Http(404).to_string(),
        "the logo URL returned HTTP 404"
    );
    assert_eq!(
        LogoError::NotAnImage.to_string(),
        "the logo isn't a PNG or ICO image"
    );
}
