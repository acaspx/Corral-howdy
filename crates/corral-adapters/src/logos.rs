//! Provider logos, fetched at runtime (ADR-015, docs/07 §8.1). The repo ships no third-party
//! marks: when a provider's key is connected, Corral downloads that provider's own icon from
//! its own site and shows it in the drop. Each logo has a [`LogoId`], which names its file.
//!
//! A logo is untrusted input (security model T19):
//! - **Where from:** one fixed HTTPS URL per logo ([`logo_source`]), on the owner's own hosts
//!   (or the owner's folder on a shared image host). Redirects are followed only to HTTPS on
//!   those hosts and under that path, at most three. Nothing is sent
//!   but the request itself: no key, no cookies.
//! - **What:** PNG or ICO only, checked by their headers, not by what the server says. At most
//!   [`MAX_LOGO_BYTES`]; the download stops as soon as it's over. A PNG may be at most
//!   [`MAX_LOGO_PIXELS`] on a side.
//! - **Decoding** is done by ImageIO in Swift, as a bounded thumbnail; nothing here decodes
//!   pixels.

use crate::transport::BoxFuture;
use corral_secrets::Provider;
use futures_util::{Stream, StreamExt};
use std::fmt::Display;
use std::time::Duration;

/// The largest logo accepted, in bytes.
pub const MAX_LOGO_BYTES: usize = 1024 * 1024;
/// The most pixels on either side of a PNG logo.
pub const MAX_LOGO_PIXELS: u32 = 1024;
/// Redirects followed for one logo.
const MAX_REDIRECTS: usize = 3;
/// How long one logo download may take.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Whose logo: names the file it's kept in and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogoId {
    /// A provider's agent (Grok, Claude…).
    Provider(Provider),
}

impl LogoId {
    /// The file name it's kept under, without the extension: lowercase letters, digits and `-`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Provider(provider) => provider.id(),
        }
    }

    /// For messages.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Provider(provider) => provider.display_name(),
        }
    }
}

impl From<Provider> for LogoId {
    fn from(provider: Provider) -> Self {
        Self::Provider(provider)
    }
}

/// Where a logo comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogoSource {
    /// The fixed HTTPS URL.
    pub url: &'static str,
    /// The hosts the URL and any redirect may be on.
    pub hosts: &'static [&'static str],
    /// The path the URL and any redirect must start with: `/` on the owner's own site, the
    /// owner's folder on a shared image host.
    pub path_prefix: &'static str,
}

/// The provider's logo source, if Corral knows one: an icon the provider's own site publishes,
/// checked on 2026-11-12. OpenAI has none: its sites refuse requests that don't come from a
/// browser, so GPT's drop keeps its monogram.
pub fn logo_source(id: impl Into<LogoId>) -> Option<&'static LogoSource> {
    // xAI's mark (48 px ICO); grok.com publishes no icon outside its web app.
    const XAI: LogoSource = LogoSource {
        url: "https://x.ai/favicon.ico",
        hosts: &["x.ai", "www.x.ai"],
        path_prefix: "/",
    };
    // Claude's mark (180 px PNG).
    const CLAUDE: LogoSource = LogoSource {
        url: "https://claude.ai/apple-touch-icon.png",
        hosts: &["claude.ai", "www.claude.ai"],
        path_prefix: "/",
    };
    // Meta AI's mark (48 px ICO).
    const META_AI: LogoSource = LogoSource {
        url: "https://www.meta.ai/favicon.ico",
        hosts: &["www.meta.ai", "meta.ai"],
        path_prefix: "/",
    };
    match id.into() {
        LogoId::Provider(Provider::Xai) => Some(&XAI),
        LogoId::Provider(Provider::Anthropic) => Some(&CLAUDE),
        LogoId::Provider(Provider::OpenAi) => None,
        LogoId::Provider(Provider::Meta) => Some(&META_AI),
    }
}

/// A logo's image format, from its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoFormat {
    /// PNG.
    Png,
    /// Windows icon (what `favicon.ico` usually is).
    Ico,
}

impl LogoFormat {
    /// The file extension it's stored with.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Ico => "ico",
        }
    }
}

/// A checked logo, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logo {
    /// Whose logo.
    pub id: LogoId,
    /// Its format, from its header.
    pub format: LogoFormat,
    /// The file's bytes.
    pub bytes: Vec<u8>,
}

/// Why there's no logo.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogoError {
    /// Corral knows no logo for this provider.
    #[error("there's no logo source for this provider")]
    NoSource,
    /// Over [`MAX_LOGO_BYTES`] (the size seen when the download stopped).
    #[error("the logo is larger than 1 MB")]
    TooLarge(usize),
    /// A PNG with no pixels, or more than [`MAX_LOGO_PIXELS`] on a side.
    #[error("the logo is {width} × {height} pixels; it must be 1 to 1024 on each side")]
    TooManyPixels {
        /// Width, from the header.
        width: u32,
        /// Height, from the header.
        height: u32,
    },
    /// Not a PNG or ICO, whatever the server called it.
    #[error("the logo isn't a PNG or ICO image")]
    NotAnImage,
    /// The server answered with something other than 200.
    #[error("the logo URL returned HTTP {0}")]
    Http(u16),
    /// A redirect left the provider's HTTPS hosts, or there were too many.
    #[error("the logo URL redirected off the provider's site")]
    Redirect,
    /// The network failed.
    #[error("couldn't fetch the logo: {0}")]
    Fetch(String),
}

/// Checks a logo's bytes: size, then a PNG or ICO header, then a PNG's pixel size.
pub fn check_logo(bytes: &[u8]) -> Result<LogoFormat, LogoError> {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(LogoError::TooLarge(bytes.len()));
    }
    if bytes.starts_with(&PNG_SIGNATURE) {
        // The first chunk must be IHDR: length, "IHDR", width, height (big-endian).
        let (Some(name), Some(width), Some(height)) =
            (bytes.get(12..16), be_u32(bytes, 16), be_u32(bytes, 20))
        else {
            return Err(LogoError::NotAnImage);
        };
        if name != b"IHDR" {
            return Err(LogoError::NotAnImage);
        }
        let fits = |side: u32| (1..=MAX_LOGO_PIXELS).contains(&side);
        if !fits(width) || !fits(height) {
            return Err(LogoError::TooManyPixels { width, height });
        }
        return Ok(LogoFormat::Png);
    }
    // ICO: reserved 0, type 1, then 1 to 64 directory entries of 16 bytes each. Entries are at
    // most 256 px by definition; ImageIO bounds what's inside.
    if bytes.get(0..4) == Some(&[0, 0, 1, 0]) {
        let count = bytes
            .get(4..6)
            .map_or(0, |c| usize::from(u16::from_le_bytes([c[0], c[1]])));
        if (1..=64).contains(&count) && bytes.len() >= 6 + 16 * count {
            return Ok(LogoFormat::Ico);
        }
    }
    Err(LogoError::NotAnImage)
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Whether a redirect to `url` may be followed: HTTPS, the default port, no user info, one of
/// the source's hosts exactly, and a path under the source's prefix (after `.` and `..` are
/// resolved).
pub fn redirect_allowed(source: &LogoSource, url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|h| source.hosts.contains(&h))
        && url.path().starts_with(source.path_prefix)
}

/// Reads a body into memory, stopping with [`LogoError::TooLarge`] as soon as it passes `cap`.
pub async fn read_capped<S, B, E>(stream: S, cap: usize) -> Result<Vec<u8>, LogoError>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: Display,
{
    let mut stream = std::pin::pin!(stream);
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| LogoError::Fetch(e.to_string()))?;
        let chunk = chunk.as_ref();
        let total = body.len().saturating_add(chunk.len());
        if total > cap {
            return Err(LogoError::TooLarge(total));
        }
        body.extend_from_slice(chunk);
    }
    Ok(body)
}

/// Downloads a logo's bytes. Fakes answer in tests (AGENTS.md rule 7).
pub trait LogoFetcher: Send + Sync {
    /// GETs `source.url`, following only redirects [`redirect_allowed`] allows, and returns at
    /// most [`MAX_LOGO_BYTES`].
    fn get<'a>(&'a self, source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>>;
}

/// Fetches and checks a logo.
pub async fn fetch_logo(
    fetcher: &dyn LogoFetcher,
    id: impl Into<LogoId>,
) -> Result<Logo, LogoError> {
    let id = id.into();
    let source = logo_source(id).ok_or(LogoError::NoSource)?;
    let bytes = fetcher.get(source).await?;
    let format = check_logo(&bytes)?;
    Ok(Logo { id, format, bytes })
}

/// The real network, over HTTPS only. Each download gets its own client, so its redirect rule
/// knows the source's hosts; logos are fetched rarely.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReqwestLogoFetcher;

impl LogoFetcher for ReqwestLogoFetcher {
    fn get<'a>(&'a self, source: &'a LogoSource) -> BoxFuture<'a, Result<Vec<u8>, LogoError>> {
        let source = *source;
        Box::pin(async move {
            let policy = reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error(LogoError::Redirect)
                } else if redirect_allowed(&source, attempt.url().as_str()) {
                    attempt.follow()
                } else {
                    attempt.error(LogoError::Redirect)
                }
            });
            let client = reqwest::Client::builder()
                .https_only(true)
                .redirect(policy)
                .timeout(TIMEOUT)
                .user_agent(concat!("Corral/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| LogoError::Fetch(e.without_url().to_string()))?;
            let response = client
                .get(source.url)
                .header(
                    reqwest::header::ACCEPT,
                    "image/png,image/x-icon;q=0.9,image/*;q=0.5",
                )
                .send()
                .await
                .map_err(|e| {
                    if e.is_redirect() {
                        LogoError::Redirect
                    } else {
                        LogoError::Fetch(e.without_url().to_string())
                    }
                })?;
            let status = response.status().as_u16();
            if status != 200 {
                return Err(LogoError::Http(status));
            }
            if let Some(length) = response.content_length() {
                let length = usize::try_from(length).unwrap_or(usize::MAX);
                if length > MAX_LOGO_BYTES {
                    return Err(LogoError::TooLarge(length));
                }
            }
            read_capped(response.bytes_stream(), MAX_LOGO_BYTES).await
        })
    }
}
