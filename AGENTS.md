# AGENTS.md: Rules for coding agents working on Corral

> `CLAUDE.md` is a symlink to this file. Keep this file short; details live in `docs/`.

## What Corral is
A macOS desktop app (Tauri 2 + React/TS + Rust core + a Swift/Metal layer for the liquid-glass corral) for deploying and supervising AI agents. Read `docs/00-product-brief.md` and `docs/01-system-design.md` before your first change; read `docs/07-brand-and-corral-surface.md` before touching `native/`.

## Non-negotiables
1. **Tests first.** Commit the failing acceptance tests named in the issue *before* implementation. Don't change or delete existing test code (including inline `#[cfg(test)]` modules, which stay at the end of the file) unless the PR has a "Test changes" section explaining why. Adding tests is always fine.
2. **Stay in scope.** Only touch the issue's allowed paths (plus tests/fixtures). If you must go wider, stop and say so in the PR.
3. **Safety logic lives only in `corral-core` / `corral-policy`.** Never put allow/deny, budget or lifecycle decisions in the UI, Tauri shell or Swift.
4. **No side effect without the gate.** Tools are called only through `ToolGateway` with a `Decision::Allow` token. Never add a bypass "for tests"; use `FakeAdapter` scenarios.
5. **Never log, persist or print secrets.** Keys come from the Keychain only.
6. **No unofficial consumer-agent integrations** (Muse, dots). See ADR-005.
7. **No real API keys or network calls in tests.** Use cassettes and fakes.
8. **Exhaustive matches** on `ActionClass` and `Event` kinds: no `_ =>` wildcard arms.
9. You may not merge. After 3 red CI cycles, stop, label `needs-human`, and write what you tried.

## Commands
```
just check        # fmt + lint + typecheck + unit + integration + eval gate (run before every push)
just test         # Rust + TS tests
just eval gate    # deterministic safety/cost/control evals
just dev          # run the app
just storybook    # UI components with fixture event streams
just native-test  # xcodebuild test for CorralNative (shape grammar + render snapshots)
```

## Code style
- Rust: `clippy -D warnings`, `thiserror` for errors, no `unwrap()` outside tests, no `unsafe` without an ADR.
- TS: strict mode, no `any`, components get a story for each state.
- Names match the docs (Run, Session, Checkpoint, ActionClass, PolicyGate…).

## PRs
Use the PR template. Link the issue, list test IDs, paste the `just check` summary, and fill in "Test changes" and "Docs changes" (write "none" if none).

## Protected paths (human code-owner review required)
Safety logic: `crates/corral-policy/**` · `crates/corral-tools/src/fs/**` · `crates/corral-core/src/budget/**` · `apps/desktop/src-tauri/capabilities/**` · `evals/safety/**`
Loop integrity (what decides whether a PR passes): `.github/**` · `scripts/ci/**` · `justfile` · `Cargo.toml` · `clippy.toml` · `deny.toml` · `rust-toolchain.toml` · `crates/corral-evals/**` · `evals/reports/baselines/**` · `AGENTS.md`
Never change these to make your own PR pass. If one is wrong, stop and say so in the PR.

## Rules learned
<!-- Add one line per lesson from review findings. Newest first. Format: YYYY-MM-DD — rule — (PR #) -->
- 2026-11-07 — Every CI `run` step uses bash with pipefail; never pipe a check into `tee` without it. — (adversarial review)
- 2026-11-07 — A check must never read its rules or baseline from the PR it is judging; read them from the base commit. — (adversarial review)
- 2026-11-07 — Data that crosses into Swift (IDs, text) is untrusted: dedupe and bound it; never trap on bad input. — (adversarial review)
