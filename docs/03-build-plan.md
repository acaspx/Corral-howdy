# Corral: Build Plan

Status: Draft v0.1 · Last updated: 2026-11-09
How work flows through agents and reviewers: [06-loop-engineering.md](06-loop-engineering.md). What "done" means for tests: [04-test-plan.md](04-test-plan.md).

## How to read this plan
- Work is cut into **milestones** (M0–M9). Each has **exit criteria that are tests or evals**, not opinions.
- Each milestone breaks into **issues** small enough for one agent to finish in one loop (target: ≤ ~400 changed lines excluding tests and fixtures).
- **Spikes** are time-boxed investigations that end in a written finding plus an ADR update. No production code is merged from a spike.
- Durations are rough calendar estimates for an agent-built loop with one human reviewer (the owner). Review bandwidth, not typing speed, will be the bottleneck. Re-plan after M2 using real loop metrics.

## Next up (November 2026)

Re-planned 2026-11-09 around a **walking skeleton**: one real agent working live on the desktop, through every layer, before any layer is finished. It surfaces the hard problems (the bridge, streaming, live updates) while they're still cheap to fix. Work top to bottom; a step is done when its tests pass, not when the code looks right. Test IDs are in [04-test-plan.md](04-test-plan.md).

### 1. Finish M0 (this week)
| Step | Tracked in | Done when |
|---|---|---|
| ✅ `main` ruleset: pull request, six required CI jobs, squash only, no force push or deletion, empty bypass list | Settings | Done 2026-11-08 |
| ✅ Desktop playground | #17 | Merged |
| Eval runner fails closed on a missing `--baseline` | Issue + PR | TP-EVAL-07..08 |
| CI keeps runs on `main` instead of cancelling them | Issue | Two quick merges both finish |
| Quit, logout and Dock reopen behave | Issue | TP-SHELL-04..06 |
| Narrow the main window's `core:default` permissions | Issue | TP-IPC-05 |
| Prove the M0 exit | The PRs above | All jobs green in < 10 min; a PR touching a protected path requests the code owner |

### 2. Walking skeleton: Grok, live on the desktop (about 4 weeks)
Goal: add an xAI key, click Grok's drop, ask a question, watch the ring turn and the hover card count time and spend, read the answer, send Grok back. Journey **J0**.

**Safety while the policy engine doesn't exist yet:** the skeleton agent gets **no tools at all**. Requests carry no tool definitions and provider-side tools are off, so there's nothing to approve and nothing can act on your behalf. The key comes from the Keychain at request time, and every run has a hard spend cap. Tools and checkpoints arrive with M2.

| Slice | Builds on | Done when |
|---|---|---|
| **SK-1** Runs and events: event types (#8), lifecycle (#9), event store (#10), supervisor with cancel (M1 items 1–3 and 5) | M0 | TP-LIFE-\*, TP-STORE-\* |
| **SK-2** Bridge: Spike S1 (#3), then the real roster and run snapshots streaming to Swift, intents back | S1 | TP-BRIDGE-01..04, TP-VIEW-01..05 |
| **SK-3** Keychain and a minimal "add your xAI key" form in the main window (M3 item 1) | SK-1 | TP-SEC-01..03, TP-SETUP-01, TP-SKEL-01 |
| **SK-4** Grok, text only: streaming `openai_responses` client, no tools sent, provider tools off, cassettes for CI (M3 items 2 and 5, minus tools) | SK-1 | TP-ADP-01, 03, 04, 05; TP-SKEL-02 |
| **SK-5** Hard spend cap per run: the cap half of the budget meter (M2 item, protected path) | SK-4 | TP-BUD-01, TP-BUD-05 |
| **SK-6a** Corral on real events, core half: run cards (elapsed, spend, the cleaned answer, buttons by state), controls for deploy from a drop, Stop, Send back and Retry, the cards and intents on the bridge, and the app wired to Grok with the event store and a $0.25 cap | SK-2, SK-4, SK-5 | TP-SKEL-03..05 (core and wire) |
| **SK-6b** Corral on real events, SwiftUI half: each drop's card (hover opens it, a click pins it with the cursor in its task field; ×, Esc or a click elsewhere closes it; 07 §8.2), the working ring and status badges, Stop, Send back and Retry | SK-6a, S6 | TP-SKEL-03..05 (on screen), TP-HOVER-01, 02, 05 |
| **SK-7** Claude, text only: a streaming `anthropic_messages` client on the same retry, Stop and budget path as Grok (`stream.rs`), no tools sent, `max_tokens` from the remaining budget, cost computed from tokens, hand-written cassettes; Claude's drop runs it once an Anthropic key is connected (pulled forward from M5 item 1) | SK-4, SK-5, agents from keys (#52) | TP-ADP-01, 03, 04, 05; TP-SKEL-02; TP-BUD-01, 05 (for Claude) |
| **Logos** Provider logos in the drops: fetched once from each provider's own site when its key is connected, checked, kept in Corral's folder, drawn by ImageIO (ADR-015) | Agents from keys (#52) | TP-LOGO-05..10 |
| **Linked apps** Consumer Muse and your dot as linked drops (open-in-app only), turned on from the corral's right-click menu, with their pictures; pulled forward from M5 item 7 (ADR-005) | Logos | TP-LINK-01..05 |

**Exit:** J0 green in CI against a cassette, and the owner runs it once live with a $0.25 cap. Spikes S1 and S5 run during SK-1. **The owner ran J0 live on 2026-11-13.**

### 3. Then grow the skeleton
1. **Rest of M1:** ~~`FakeAdapter`, Recall-all, crash recovery~~ done (FakeAdapter and Stop all agents 2026-11-13; Recall-all and crash recovery were already in the core). Pending safety scenarios now fail the gate (TP-EVAL-09..10, 2026-11-14)
2. **M2:** tools, the policy engine and checkpoints; approvals appear in the hover card
3. **Rest of M3:** Grok's tools through the gate, nightly live smoke
4. **M4:** the full corral UI: setup flow, prompt bar, trace tree, Compare
5. **M5:** Claude Code, Devin and other CLI agents over ACP, Claude and Muse Spark APIs, OpenAI Agents

### Alongside
| Work | Issue | When |
|---|---|---|
| **S5** Glass on real hardware, with the playground: screenshots at 0–8 agents over light, dark and busy wallpapers; TP-GLASS-05 in Activity Monitor | #7 | During SK-1 |
| **S6** Hover card, trace tree, logos in SwiftUI | #12 | Before SK-6 |
| **S2** UI test harness | #4 | Before M4 |
| **S4** ACP client | #6 | Before M5 |
| Design to spec: setup flow, prompt bar, done card, Compare into docs/07 and docs/01 (TP-SETUP-01..04, TP-HOVER-10, TP-COMPARE-01..03, J12). Prompt bar ✅ (2026-11-20, 07 §6.1, TP-BAR-01..07) | Design issue | Before M4 |

### Not now
Voice (M6), native extras (M7), sync (M9), and any provider beyond Grok until J0 is green.

## Critical path

```
M0 ──► M1 ──► M2 ──► M3 ──► M4 ──► M8 (v1.0 release)
              │       └──► M5 ──┘
              └──► M6 (voice, parallel after M2)
                    M7 (native extras, parallel after M4)
                                       M9 (v1.1: sync + A2A)
Spikes S1–S6 run during M0–M1.
```

The core (M1–M2) is built and proven **with a fake agent before any real provider exists**. That way the safety machinery is tested in isolation, and every real adapter added later must pass the same contract tests.

---

## Spikes (during M0–M1)

| ID | Question | Output |
|---|---|---|
| S1 | Can a Tauri 2 app embed a Swift package (swift-rs) for a non-activating NSPanel, actionable notifications and Touch ID, and ship a WidgetKit extension in the same signed bundle? Should the core run in-process or as a separate helper process, so a UI crash can't stop running agents (security model T26)? | Working toy app + ADR-001 confirmed or amended + process-model decision. **Do this spike first:** all of M4 depends on it. **Done 2026-11-10:** bridge, notifications and Touch ID proven on a Mac; findings in [adr/001-stack.md](adr/001-stack.md); code on `spike/s1-bridge` |
| S2 | UI end-to-end testing on macOS: what's the most reliable way to drive the real app in CI (WebDriver support, accessibility-API scripting, or Playwright against the frontend with a mocked IPC bridge)? | Test harness decision recorded in the test plan |
| S3 | OpenAI Agents API: does it expose per-tool approval hooks or only up-front tool restriction? Same question for the Meta Model API's agent features and xAI server-side tools | ✅ Findings 2026-11-07: [spikes/S3-provider-approval-hooks.md](spikes/S3-provider-approval-hooks.md), ADR-013. Cassettes pending keys |
| S5 | Liquid-glass corral on macOS 26: ~~compile `CorralGlass.metal` + `CorralGlassView`~~ (done 2026-11-07: builds and TP-SHAPE/TP-RANK pass in CI on `macos-26`); pick how system glass and the Metal pass combine (plan A: shader over glass layer, B: merged glass shapes in a `GlassEffectContainer` + Metal highlights, C: wallpaper fallback); tune tokens against the reference image; measure GPU frame time; check that polar morphs read well during usage reorders | Signed-off render at n = 0–8, token file, ADR-009/010 confirmed |
| S4 | ACP client in Rust: drive Claude Code, Codex CLI, Gemini CLI and the Devin CLI through the Rust ACP crate; map their permission requests to checkpoints. For Devin also find out: which actions ask (Cognition documents shell commands; file edits?), whether Corral can set its permission mode, how its spend is reported (it bills in ACUs, not tokens) for the budget meter, and whether cloud Devin sessions (ACP over WebSocket, on Cognition's machines) ask before each action or only report `blocked` | Toy client + list of permission request shapes + a Devin note: gating level for local and cloud sessions, and how its spend maps to the budget |
| S6 | Hover card + trace tree in SwiftUI: hover tracking in a non-activating panel (does `onContinuousHover` work without focus?), a second click-through panel for the tree, height budget on a 13" display with 3 columns, paper/veil materials over busy wallpapers, logo import (ImageIO, SVG rasterize) | Working prototype matching `corral-shape-preview.html`, token file update, panel layout decision |

---

## M0: Foundations (~1 week)
Goal: a repo where agents can work safely from day one.

- Monorepo skeleton (see system design §13); Cargo workspace; pnpm workspace
- `AGENTS.md` (and `CLAUDE.md` → symlink), `justfile` with `just check`, PR/issue templates, CODEOWNERS
- CI on macOS runners: fmt, clippy (`-D warnings`), tsc, eslint, `cargo nextest`, vitest, `cargo-deny`, gitleaks
- `SECURITY.md`, `CONTRIBUTING.md`, LICENSE (decision pending)
- Tauri 2 hello-world with tray icon + main window; `CorralNative` package builds and tests in CI (`xcodebuild`, Xcode 26 / macOS 26 SDK); app deployment target macOS 26
- `corral-evals` crate stub that runs an empty suite and reports JSON

**Exit:** a no-op PR passes all CI jobs in < 10 min; a PR touching a protected path requests the human code owner automatically.

**Status (2026-11-08):** CI hardened (pinned actions, fail-closed checks, base-commit baselines, test-change guard) and green. Remaining: finish the `main` ruleset, the four small M0 fixes, then prove the exit with a no-op PR. See [Next up](#next-up-november-2026).

## M1: Core runtime with a fake agent (~2 weeks)
Goal: deploy → run → complete → send back, headless, fully event-sourced.

Issues (examples):
1. Domain types + serde schema for `Event` v1 (golden JSON fixtures)
2. Lifecycle state machine `next(state, event)` + property tests
3. SQLite event store: append, read by run, hash chain, verify
4. Projections: `runs_view`, `spend_view`, rebuild-from-scratch equivalence test
5. Supervisor + run actors, concurrency limit, cancel
6. `FakeAdapter` driven by scenario YAML (see `evals/scenarios/`)
7. Recall-all with forced-drop after 2 s
8. Crash recovery: orphaned runs → `Failed{app_restarted}`

**Exit:** TP-LIFE-*, TP-STORE-*, TP-SUP-* green; EV-STOP-01 passing (Recall-all p95 < 2 s with 8 fake runs; a gate scenario since 2026-11-23, with EV-STOP-04 for adapters that ignore Stop). Pause and resume in the core, with EV-STOP-03, came 2026-11-23 (TP-SUP-04..06); the card's Pause and Resume buttons, with Pausing and Paused states on the card and the drop, followed (TP-PAUSE-01..05).

## M2: Policy engine and checkpoints (~2 weeks)
Goal: the safety core, proven before any real agent can touch it.

1. ✅ Action class enum (exhaustive) + profile TOML schema + default profiles (2026-11-13)
2. ✅ Pure `decide(profile, action, ctx)` + table-driven tests for every class × decision; allow-for-run, destination pinning and taint included (2026-11-13; 37 of 37 mutants caught); approve once per exact call for normal risk ✅ 2026-11-18 (TP-POL-08)
3. ✅ `PolicyGate` + `Decision::Allow` capability token required by `ToolGateway` (2026-11-14: `Gate`, `AllowToken`, `ToolGateway` with class checks, output cap and taint)
4. Checkpoint lifecycle: open → resolve (approve / deny / edit / allow-for-run) → timeout behavior (core ✅ 2026-11-14: `ToolRunner` and `CheckpointDesk`, driven by the FakeAdapter; hover card ✅: the approval box with Approve and Deny over IPC; allow-for-run on the card ✅ 2026-11-15; edit-then-approve in the core ✅ 2026-11-17; the card's edit fields in the core ✅ 2026-11-17; the edit sheet on the card ✅ 2026-11-17; Touch ID for critical on the card ✅ 2026-11-18)
5. Budget meter: pre-request estimate, hard cap, soft-limit checkpoint, reconciliation (soft limit ✅ 2026-11-15: a `budget.threshold` checkpoint on the card; set at $0.10 in the app since Grok can ask, M3; monthly caps per agent and for all agents ✅ 2026-11-18, seeded from the store at launch)
6. Filesystem sandbox (canonicalization, symlink escapes) (✅ 2026-11-14: `corral_tools::fs` with `fs.read`, `fs.write` and `fs.delete`; not registered for any agent until M3)
7. Loop detector (repeat tool + args → pause)
8. Safety eval suite v1 on fake agents (injection-shaped scenarios) (✅ 2026-11-14: the runner executes scenarios on the FakeAdapter through the real policy, desk and gateway; a pending safety scenario fails the gate; EV-SAFE-01..05 and 26 pass; the injection corpus EV-SAFE-01..20 and EV-SAFE-21 ✅ 2026-11-18)

**Exit:** TP-POL-*, TP-FS-*, TP-BUD-* green; **EV-SAFE gate = 100% recall** on the must-catch set (✅ 2026-11-18: EV-SAFE-01..21 and 26, 22 scenarios, all in the gate baseline); EV-COST-01..03 passing (✅ 2026-11-18, in the gate baseline).

Loop detector (item 7) ✅ 2026-11-15: in the policy gate; from the `loop_limit`-th identical call on (5 by default), each one waits at a checkpoint (TP-LOOP-01..06, EV-COST-02).

## M3: First real provider, xAI Grok (~1.5 weeks)
1. Keychain secrets store + settings UI stub (✅ SK-3)
2. `openai_responses` client: streaming, tool calls, usage, errors, retries with backoff (✅ SK-4, except tool calls, which come with item 3)
3. `native` adapter: agent loop over the client, all local tools via the gate. In four PRs:
   1. ✅ Tools describe themselves to a model and go out under wire names (2026-11-18, TP-TOOL-01..03)
   2. ✅ The Responses request offers the agent's tools and carries the conversation as input items; the decoder reads the model's function calls instead of failing the run (2026-11-18, TP-TOOL-04..06)
   3. ✅ The loop: each turn admitted by the budget, every call through `ToolRunner` (checkpoints on the card), results back to the model, Stop and the soft limit, a cap on turns (2026-11-18, TP-TOOL-07..12; `ResponsesAgent`)
   4. ✅ In the app: Grok gets the file tools in `~/Corral/Grok`, random checkpoint IDs, and a $0.10 soft limit (2026-11-18, TP-TOOL-13..14)
4. Provider-side tools toggle (`provider_tools` permission). In two PRs:
   1. ✅ Claude's web search, offered only when the policy allows `provider_tools`: in the trace, in the budget, tainting the run, with sources in the answer (2026-11-19, TP-PROV-01..09, TP-GATE-08). Grok's waits for an xAI recording
   2. ✅ The switch in the main window, per agent, saying where the tool runs, what it costs and that its results are untrusted; read as each run starts (2026-11-19, TP-PROV-10..14)
5. Record/replay cassettes for provider tests (no live keys in CI)
6. Adapter contract suite runs against `fake` and `native+xai(cassette)` (✅ 2026-11-18: `tests/contract.rs`, also Grok text-only and Claude)

**Exit:** contract suite green for xAI; nightly live smoke (protected env, $1 cap) green; EV-COST-04 (spend within 2% of provider-reported).

Progress: the nightly live smoke is wired (2026-11-19: `.github/workflows/live-smoke.yml`, `corral_adapters::smoke`, TP-SMOKE-01..04, EV-PROV-01 and 03) and checks Claude until there's an xAI key. EV-COST-04 waits for that key too: xAI reports what it billed with each response, while Anthropic reports only tokens, so checking Claude's dollars would need an Admin API key in CI (decided 2026-11-19: wait for xAI).

## M4: The corral UI (~3.5 weeks)
1. ✅ Typed IPC + the Rust → Swift snapshot/delta bridge (C ABI). Typed IPC ✅ with ts-rs (2026-11-20, ADR-001 §6, TP-IPC-06); the bridge has been in since SK-2
2. ✅ **Liquid-glass corral** (per S5): `CorralPanel` at bottom center, shape grammar up to an octagon, usage-ranked slots (top-left, clockwise), polar morph springs, drop tokens with agent logos and status rings, deploy (click / drag up / selected / all); working agents stay in the shape (ADR-014)
2a. **Usage ranking in the core:** `usage_rank` projection, idle-gated publishing with hysteresis, "Keep order fixed" setting
2b. **Shown set:** "Your corral holds up to 8" chooser when a 9th agent is added + Settings → Corral; hidden agents reachable via `@` in the prompt bar

    Progress: the logic of both is in the core (2026-11-20, `corral_core::usage_rank`, TP-USAGE-01..05): the score, the rank, `reorder` with 10% hysteresis, `UsageOrder` (idle-gated, "Keep order fixed") and `ShownSet` (the first 8, a 9th asks, no auto-promotion). The app now keeps them in `corral-order.json` (2026-11-20, `corral_order.rs`, TP-USAGE-06..09): the corral opens most used first, seeded with the last 90 days of deploys (`Store::deploys_since`), shows at most 8, and agents join and leave at once. Swift now reports when the corral turns idle or busy (2026-11-20, `corralIdle`, TP-USAGE-10..12), so a new order also applies while Corral runs, once the corral is idle and no checkpoint waits on the user. The main window's Corral section has the chooser and "Keep order fixed" (2026-11-20, `corral_settings.rs`, TP-USAGE-13..16). How `@` addresses a prompt, hidden agents included, is in the core (2026-11-20, `corral_core::mention`, TP-PROMPT-01..05); the prompt bar that uses it is specced (2026-11-20, 07 §6.1, TP-BAR-01..07). Its rules are in the core (`corral_core::prompt_bar`), its wire is ABI 3 (`prompt_bar_feed.rs`, TP-BAR-08), and its panel opens from a double-click on the corral or a drop dragged up (2026-11-21, `native/CorralNative/Sources/CorralNative/Bar`). The summon shortcut (⌃⌥Space, changeable from the corral's menu) toggles it (2026-11-21, TP-BAR-09). Each suggestion shows its agent's logo (2026-11-22, TP-BAR-10). Item 2 is done.
3. ✅ **Hover cards + trace tree** (per S6; spec in [07 §8](07-brand-and-corral-surface.md)): paper hover card with plan progress, elapsed, spend and state-based buttons; trace tree of translucent prompt/task/subtask cards wired to drops; `plan_view` projection + `plan.updated` events in the core; logo import
    Progress: `plan_view` is in the core (2026-11-22, TP-TRACE-01, #115). The core picks the trace tree's columns (`corral_core::trace_tree`) and the roster carries them and each card's plan (2026-11-22, TP-TRACE-02, 06, TP-HOVER-03). Swift draws the card's progress, and the columns with their wires above the corral (2026-11-22, TP-HOVER-03, TP-TRACE-02..04). A task card opens its run in the trace viewer and a click fans out the stack (2026-11-22, TP-TRACE-05). Item 3 is done.
4. ✅ Checkpoint UI: approval box in the hover card (stepper for several pending, Touch ID for critical) + actionable notification; edit-args flow; nonce-bound approve; 300 ms input guard
    Progress: the approval box, Touch ID, the edit sheet, the single-use nonce and the 300 ms guard shipped in M2. The stepper steps through several waiting checkpoints, and an answer may be for any of them on that card (2026-11-22, TP-CHK-19..21). Each waiting checkpoint now raises a macOS notification with a single-use nonce: Approve inline for normal risk only; Deny; Open, which pins the card stepped to it (2026-11-22, TP-CHK-22..25). Posting needs the bundled app; `just dev` runs a bare binary, where macOS refuses notifications.
5. Main window: trace viewer (tree + waterfall), JSON copy. In two PRs:
   1. ✅ The core: `corral_core::trace_view` derives the span tree from a run's events, and `Store::recent_runs` lists runs in one query (2026-11-20, TP-TV-01..10)
   2. ✅ The viewer in the main window: the run list, the tree with its waterfall, filter by kind, the picked span's details, copy as JSON (2026-11-20, TP-TV-11..16)
6. ✅ Agent editor + policy editor (shows effective decision per class) (2026-11-25)
    Progress: decided 2026-11-24: a policy per agent (rules per class, allowed domains, checkpoint timeout, loop limit); the agent editor covers name and folder, model, monthly cap and provider tools. The policy editor's core and each agent's saved policy, loaded at launch and failing closed, came 2026-11-24 (TP-PED-01..06), and the policy editor screen, applying from each agent's next run, 2026-11-25 (TP-PED-07..09). Long-prompt prices came first, since a model offered in the editor may have one (TP-BUD-08), then each agent's model, from a priced table, and its own monthly cap up to the all-agents cap (TP-AED-01..04). Then names, 2026-11-25: a rename shows on the agent's card and in the main window, and its folder follows the name (TP-ANM-01..04). Last, the agent editor screen, 2026-11-25: name, model, monthly cap and spend, and the agent's provider tools, a save checked whole and applied without a restart (TP-AED-05..09).
7. ✅ Storybook with fixture event streams for every component state (2026-11-25: `just storybook`; every main-window component has a story per state, the trace viewer's from runs played through the supervisor and the store, and every story renders and plays in `pnpm test`, TP-SB-01, 02)
8. Accessibility pass (keyboard-only deploy/approve/deny; VoiceOver labels)
    Progress: the keyboard path through the corral: ↓ from the prompt bar, ← → between drops in rank order, Space opens the card pinned with focus on its first button (Approve when it can be pressed), Esc back to the drop, then to the bar; VoiceOver reads drops in rank order and the card as a dialog (2026-11-23, TP-A11Y-01..03, TP-HOVER-07). The main window, 2026-11-25 (TP-A11Y-04..06): axe finds nothing in any story or in the whole window, landmarks included; Tab reaches every control in order; a focused control shows a 2px ring, and nothing else draws an outline (the selected run used to). Still to check by hand: VoiceOver end to end, contrast over test wallpapers (TP-GLASS-03, TP-LOGO-03), and the main window's contrast over the stories (`just storybook`; jsdom can't draw). Tab moves between buttons in the main window only with macOS's "Keyboard navigation" setting on, as in every Mac app.

**Exit:** E2E journeys J1–J6 and J9–J11 (test plan §6) green; TP-SHAPE-*, TP-RANK-*, TP-USAGE-*, TP-GLASS-*, TP-PLACE-*, TP-HOVER-*, TP-TRACE-*, TP-LOGO-* green; axe zero serious violations; performance budgets met (corral summon p95 < 150 ms, event→UI p95 < 100 ms, idle corral 0 frames, morph < 2 ms GPU).

## M5: More adapters (~3 weeks; issues parallelize well)
1. `anthropic_messages` client + native agents on Claude models (client ✅ SK-7; tools through the shared agent loop ✅ 2026-11-19, pulled forward, TP-TOOL-16..18)
2. Meta Model API (Muse Spark) through the OpenAI-compatible client (config + cassettes)
3. `managed` adapter for the OpenAI Agents API (scope per Spike S3)
4. `acp` adapter: spawn, process group, permission mapping, kill on cancel (per Spike S4). Agents: Claude Code, Codex CLI, Gemini CLI and Devin (local sessions through the Devin CLI first; cloud sessions only if S4 shows they can be gated)
5. Custom agent manifests: parser, import flow with permission diff, fuzzing
6. MCP client in the tool gateway (stdio + streamable HTTP), each MCP tool mapped to an action class
7. ~~"Linked app" cards for consumer Muse and dots (open-in-app only)~~ Done early (Linked apps, 2026-11-12); the "manual status" from the product brief is still to do

**Exit:** every adapter passes the contract suite; EV-SAFE gate still 100% with real-provider cassettes; FZ-01 (manifest fuzz) runs 1 h clean.

## M6: Voice (~1.5 weeks; can start after M2)
1. `corral-voice` crate reusing Handy's approach (on-device Whisper/Parakeet + VAD); model download manager
2. Push-to-talk hotkey → transcript in prompt bar (edit before send by default)
3. Voice commands: deploy, prompt, pause, stop; **cannot approve high/critical**
4. Microphone permission flow + clear in-use indicator

**Exit:** TP-VOICE-* green, including TP-VOICE-04 (voice approval of a high-risk checkpoint is impossible); WER on the command phrase set ≤ 5%.

## M7: Native extras (~1.5 weeks; after M4)
(The corral panel, hover cards and trace tree moved into M4.)
1. Multi-display behavior (the level setting shipped early: desktop by default, chosen from the corral's right-click menu)
2. Polish pass on glass motion against the brand spec
3. Actionable notifications (Approve / Deny / Open) wired to the nonce-bound IPC (✅ 2026-11-22, in M4: TP-CHK-22..25)
4. Touch ID for critical checkpoints (on the card ✅ 2026-11-18, in M2; from a notification, Open pins the card, where Touch ID approves ✅ 2026-11-22)
5. App Intents (Shortcuts: "Deploy Scout with …")
6. WidgetKit widget: a read-only corral status glance (ship only if S1 says it's clean)

**Exit:** native smoke tests green on the macOS CI runner; critical approvals require LocalAuthentication.

## M8: Release v1.0 (~1.5 weeks)
1. Developer ID signing + notarization in CI (secrets in protected environment)
2. Updater with signature verification; staged rollout channel (beta → stable)
3. Homebrew cask; GitHub Releases with SBOM + artifact attestations
4. Onboarding: first agent in < 3 min (provider pick → key → first task)
5. Docs site (from `docs/`), security review, threat-model walkthrough
6. Release-candidate soak: 72 h dogfood with ≥ 5 agents across ≥ 3 providers

**Exit:** release checklist (test plan §9) fully checked; zero open P0/P1 bugs.

## M9: v1.1, sync + remote agents
1. E2E-encrypted event replication via CloudKit private database
2. `a2a` adapter (Agent Card discovery, `input-required` → checkpoint)
3. Watch for official Muse and dots control APIs, then adapter spikes

---

## Rough timeline

| Weeks from start | Milestone |
|---|---|
| 1 | M0 + spikes kick off |
| 2–3 | M1 |
| 4–5 | M2 (M6 can start) |
| 6–7 | M3 |
| 7–10 | M4 |
| 9–12 | M5 (with M6, M7 in parallel) |
| 12–14 | M7 finish + M8 |
| ~14 | **v1.0** |

Re-estimate at the end of M2 using measured loop throughput (PRs merged per week, review turnaround).
