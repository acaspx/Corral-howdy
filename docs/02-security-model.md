# Corral: Security Model and Threat Model

Status: Draft v0.1 · Last updated: 2026-11-07

Corral holds API keys and can make agents act on a user's behalf. Treat it like a password manager that can also spend money. Every row below maps to a test or eval ID in [04-test-plan.md](04-test-plan.md) and [05-evals.md](05-evals.md).

## Assets
1. Provider API keys (direct financial exposure)
2. The user's files inside and outside sandbox roots
3. The user's outbound identity (email, messages, posts)
4. Prompt/response history (often private)
5. The integrity of the audit log (what agents actually did)

## Trust boundaries
- **Untrusted:** model output, tool output, web content, MCP server responses, imported manifests, ACP agent processes, A2A remote agents.
- **Trusted:** `corral-core` policy engine, the Keychain, explicit user gestures in Corral UI.
- **Semi-trusted:** the webview UI. It renders untrusted content, so it gets least-privilege IPC.

## Threats and controls

| ID | Threat | Control | Verified by |
|---|---|---|---|
| T1 | **Prompt injection** in a web page or file makes an agent exfiltrate data or send messages | All side effects pass `PolicyGate`; `net.write`/`comms.send` ask by default; egress allowlist per agent; checkpoint shows exact destination + payload | EV-SAFE-01..20 (injection corpus) |
| T2 | Adapter bug bypasses the gate | Tools only reachable via `ToolGateway`, which requires a `Decision::Allow` token minted by the policy engine (type-level: no token, no call) | Compile-time API design + TP-POL-10 |
| T3 | Unknown/new action type slips through | `unknown` class = deny; adapters must map exhaustively (Rust `match` without wildcard) | TP-POL-03 |
| T4 | Approval fatigue → users rubber-stamp | Precision target ≥ 90%; "allow for run" option; batching of identical low-risk asks; weekly review of checkpoint noise | EV-UX-02 |
| T5 | Voice misrecognition approves an action | Voice cannot approve high/critical | TP-VOICE-04 |
| T6 | Key theft from disk/logs/traces | Keychain only; redaction filter on every log/trace sink; secret-scanner test over all persisted bytes after eval runs | TP-SEC-01, EV-SAFE-30 |
| T7 | Malicious imported manifest grants itself broad permissions | Strict schema; import shows diff of permissions; profiles can't widen beyond global ceilings | TP-MAN-*, fuzz FZ-01 |
| T8 | Path traversal / symlink escape from sandbox roots | Canonicalize + `openat`-style checks; deny symlinks leaving root | TP-FS-01..08, fuzz FZ-02 |
| T9 | Runaway spend (loops, multi-agent amplification) | Pre-request hard caps; per-provider rate limits; loop detector (same tool+args N times) → pause | EV-COST-01..05 |
| T10 | Compromised webview calls privileged IPC | Tauri capabilities scoped per window; no remote URLs loaded; strict CSP; untrusted markdown sanitized; approve commands require a nonce issued for that checkpoint | TP-IPC-01..04 |
| T11 | Audit log tampering | Hash-chained events; verify on open; tamper → banner + read-only | TP-STORE-05 |
| T12 | Supply chain (crates, npm, Swift packages) | `cargo-deny`, `npm audit --omit=dev`, lockfiles, Dependabot, pinned GitHub Actions by SHA, SBOM per release, GitHub artifact attestations | CI `security` job |
| T13 | Tampered release binary | Developer ID signing, notarization, updater signature verification | Release checklist |
| T14 | ACP child process abuse (fork bombs, lingering processes) | Process group per run; kill on cancel; resource limits; env scrubbed to explicit allowlist | TP-ACP-05, EV-STOP-02 |
| T15 | Sync server reads user data | E2E encryption; server only sees ciphertext; keys never sync | TP-SYNC-* (v1.1) |
| T16 | Using unofficial consumer-agent wrappers leaks account credentials / breaks ToS | Product rule ADR-005: not supported | Review gate |
| T17 | Glass effect used as a pretext to capture the screen | Backdrop is system Liquid Glass (no permission), or the wallpaper file as a fallback; Screen Recording is never requested (ADR-010) | TP-GLASS-04 |
| T18 | Accidental approval from a hover card (card appears under the pointer, a stray click or keypress) | Hover never approves; Approve needs a click or Return on the focused button; buttons ignore input for 300 ms after the card appears; **high-risk Approve works only on a pinned card**, and only once any truncated detail has been expanded; critical needs Touch ID; the approve command carries the checkpoint's single-use nonce, so a stale card can't approve a newer checkpoint | TP-HOVER-04..06, TP-HOVER-09 |
| T19 | Malicious agent logo file (decoder exploits, SVG scripts or external references, huge images) | Copied into Corral's folder and never hot-linked; ≤ 1 MB and ≤ 1024 px; decoded with ImageIO; SVG rasterized once at import with scripts and external references stripped; manifest `icon` paths can't leave the manifest's folder | TP-LOGO-01..02, fuzz FZ-06 |
| T20 | **UI spoofing by agent-written text.** An injected plan step or task title impersonates the approval box ("NEEDS YOUR APPROVAL · read file · low risk") while the real checkpoint is `comms.send`; bidi controls or look-alike characters disguise a destination | The approval box is drawn only from the core's checkpoint fields, never from agent text. Agent text is drawn in its own style, length-capped, with control and bidi characters stripped. Destinations show internationalized domains as punycode and flag mixed scripts | TP-HOVER-08, EV-SAFE-37 |
| T21 | **Approval sweep.** "Allow for this run" on a high-risk class lets a later injected action of the same class through | For high and critical classes, allow-for-run is scoped to the class *and* the destination (same recipient, domain or path root). Anything new asks again | TP-POL-11, EV-SAFE-38 |
| T22 | **CLI agent pre-approves itself.** An ACP agent's own settings auto-approve tools (or it runs in a skip-permissions mode), so it never asks Corral | The ACP adapter reports `Partial` gating unless Corral sets the agent's permission mode for the session, and refuses to start an agent in a known bypass mode. The deploy sheet says which actions Corral can't see | TP-ACP-06 |
| T23 | **Programs launched outside the gate.** An MCP server or ACP command from a manifest runs with the user's full rights; the gate covers its *tool calls*, not the process | Launching a program asks once per (path, SHA-256), as an `exec.shell`-class checkpoint; the import diff shows the path and hash; a changed binary asks again | TP-MAN-04 |
| T24 | **Whole-log rewrite.** Someone with disk access rewrites the event log and recomputes every hash | Each run's final head hash is MACed with a key that lives only in the Keychain, and verified on open | TP-STORE-06 |
| T25 | **One request overshoots the hard cap** (output, reasoning tokens, vendor tool fees aren't known up front) | Every request sets its max output tokens from the remaining budget; per-call vendor tool fees are in the price table and counted before the call | TP-BUD-05 |
| T26 | **A UI crash takes the supervisor down** and orphans the processes CLI agents started (macOS has no parent-death signal) | Swift input is deduplicated and bounded so the UI can't trap on it (TP-SHAPE-08..09). The core stays in the app process for v1 (ADR-001, Spike S1): a crash stops every run, which fails safe, and crash recovery marks those runs failed on the next launch (TP-SUP-03). Either way, ACP agents start under a small reaper that kills their process group when Corral's pipe closes | TP-ACP-07 |
| T27 | **A builder agent weakens the checks that judge its PR** (edits the scope guard, the eval baseline or lint config; a pipeline hides a failing exit code) | Loop-integrity files are protected paths; CI runs the base commit's scope guard and baseline; every CI step runs with pipefail; actions pinned by SHA; toolchain pinned | TP-CI-01..13, TP-EVAL-01..06 |

## Defaults a user gets on first launch
- No agent can send, post, buy, delete, or run shell without asking.
- Sandbox root per agent: `~/Corral/<AgentName>/`.
- Per-run budget $1, monthly per agent $25, global monthly $100 (all editable).
- Telemetry: off. Trace export: off. Sync: off.

## Protected code paths (human review required, see CODEOWNERS)
**Safety logic:** `crates/corral-policy/**`, `crates/corral-tools/src/fs/**`, `crates/corral-core/src/budget/**`, `apps/desktop/src-tauri/capabilities/**`, secrets handling, `evals/safety/**`.

**Loop integrity** (the files that decide whether a PR passes; an agent that could edit them could grade its own work): `.github/**`, `scripts/ci/**`, `justfile`, `Cargo.toml`, `clippy.toml`, `deny.toml`, `rust-toolchain.toml`, `crates/corral-evals/**`, `evals/reports/baselines/**`, `AGENTS.md`. CI also reads the scope guard and the eval baseline from the PR's *base* commit, so a PR can't weaken them for itself.

## Responsible disclosure
`SECURITY.md` (to write in M0): private reporting via GitHub Security Advisories; 90-day disclosure window.
