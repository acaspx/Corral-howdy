# ADR-015: Provider logos are fetched at runtime

Status: **Accepted** 2026-11-12 (owner decision). Amends the logo clause of ADR-014.

## Context
ADR-014 said agent logos are user-assigned and that no third-party marks ship in the repo, so a
provider agent's drop showed a monogram ("G", "C"). The owner wants the drop to show the
provider's logo as soon as the key is added, without the user finding and importing an image.
Three options were weighed:

- **A.** Keep monograms until the user imports a logo (ADR-014 as written).
- **B.** Corral downloads the provider's own icon from the provider's own site when its key is
  connected. Nothing ships in the repo.
- **C.** Ship a curated "provider marks" pack in the app bundle, after checking each provider's
  brand guidelines.

## Decision
**B.** When a provider's key is connected (and at launch, for any connected provider whose logo
is missing), Corral fetches that provider's icon once, checks it, keeps it in its own folder and
shows it in the drop. A logo the user assigns later (docs/07 §8.1) takes precedence.

## Sources (probed 2026-11-12 from the owner's Mac)

| Provider | URL | Result |
|---|---|---|
| xAI (Grok) | `https://x.ai/favicon.ico` | 200, ICO, 16/32/48 px |
| Anthropic (Claude) | `https://claude.ai/apple-touch-icon.png` | 200, PNG, 180 px |
| Meta (Muse Spark) | `https://www.meta.ai/favicon.ico` | 200, ICO, 16/32/48 px |
| OpenAI (GPT) | none | `chatgpt.com` 404, `openai.com` 403 to non-browsers |

Linked apps (ADR-005) use the same downloader for their pictures, checked in the built-in browser
on 2026-11-12:

| Linked app | URL | Result |
|---|---|---|
| Muse | `https://muse.ai/favicon.ico` | 200, ICO, up to 128 px |
| Your dot (5 looks) | `https://images.ctfassets.net/8su2tbn87fck/…?fm=png&w=256` | 200, PNG, 81 to 256 px |

The dot looks are on OpenAI's dots page, served from OpenAI's folder (`8su2tbn87fck`) on the
Contentful image host. That host is shared with other companies, so the source pins the folder:
the URL and any redirect must stay under `/8su2tbn87fck/` (`LogoSource::path_prefix`).

`grok.com/apple-touch-icon.png` redirects to an HTML page, which the header check refuses, so
Grok uses xAI's mark. GPT keeps its monogram until a source works.

## Safety (security model T19, TP-LOGO-05..10)
- One fixed HTTPS URL per provider, in code (`corral_adapters::logos::logo_source`). Redirects
  are followed only to HTTPS on that provider's listed hosts, at most three. No key, cookie or
  user data is sent; the request tells the provider no more than the key check already did.
- PNG or ICO by header (never by `Content-Type`), ≤ 1 MB with the download cut at the cap, and a
  PNG ≤ 1024 px per side. Nothing in Rust decodes pixels.
- Saved as `logos/<provider id>.png|ico` in the app's data folder: the name comes from the
  provider, the bytes are re-checked, written to a temp file and renamed.
- Swift accepts only a path of that shape, opens it without following links, refuses anything but
  a regular file ≤ 1 MB, and decodes it with ImageIO as a ≤ 256 px thumbnail. Any failure shows the
  monogram; nothing traps.

## Consequences
- Provider marks appear in the user's corral but never in the repo or the app bundle.
- A provider changing or removing its icon URL means a monogram, not a failure; the table above
  is rechecked when a source breaks.
- If a provider's guidelines object to this use, its source is removed (option A for it).
