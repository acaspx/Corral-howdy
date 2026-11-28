# Spike S2: How do Corral's end-to-end journeys run in CI?

Status: Findings v1 · Issue #4 · Researched 2026-11-25 (links at the end)
Feeds: [04-test-plan.md §1, §6, §7](../04-test-plan.md), [03-build-plan.md](../03-build-plan.md) M4 exit

## The question

M4's exit asks for journeys J1–J6 and J9–J11 (test plan §6) green, and the performance budgets (§7) met. A journey crosses the whole app: the Rust core decides, the C ABI carries JSON snapshots to the Swift corral, and the user acts on the corral, the hover card, the prompt bar or the main window's web view. What's the most reliable way to drive that in CI, every PR, without real keys or network (AGENTS.md rule 7)?

## Short answer

**Test each journey as one transcript that both halves of the app are checked against, every PR. Drive the real, bundled app only for a thin nightly smoke.**

- **Every PR (blocking): journey transcripts.** A Rust test plays the journey through the real supervisor, store, policy and gateway, with `FakeAdapter` scenarios. It feeds in the user's intents and records every payload the core sends to Swift. That recording is the journey's transcript, checked in. A Swift test replays the transcript into CorralNative's real decoding and models. At each step, it performs the user's gesture on those models, as the corral's tests already do. It then checks two things: the corral emits exactly the transcript's next intent, and the step's visible state (shape, slot order, ring, card buttons, wire colour) is right. The main-window steps run in Vitest, on the same fixtures. No process is launched, no permission is needed, and it runs in today's CI jobs.
- **Nightly (non-blocking until stable): a native smoke.** XCUITest drives the bundled app for J0 and J1, built with an `e2e` feature that swaps providers for `FakeAdapter`. It also measures the performance budgets. Before anything depends on it, one proof run must show that a GitHub-hosted `macos-26` runner grants the Accessibility permission it needs; otherwise it moves to a self-hosted Mac.
- **Not chosen:** WebDriver on the web view, Playwright on a mocked frontend, and scripting the app through the Accessibility API on every PR. The first two can't see the corral. The third has the nightly's permission problem, every PR.

## What drives a Mac app, and what it can see

| Route | Corral (Swift panel, card, prompt bar) | Main window (web view) | CI on hosted macOS | Verdict |
|---|---|---|---|---|
| `tauri-driver` (official WebDriver) | ❌ web content only | ❌ on macOS: Apple ships no WKWebView driver; Windows and Linux only | n/a | Out |
| WebdriverIO `@wdio/tauri-service`, embedded provider | ❌ web content only | ✅ through a WebDriver server the plugin runs inside the app (debug builds) | ✅ no external driver | Out for now: adds an automation server to the app, and misses the corral |
| CrabNebula's `tauri-driver` fork | ❌ | ✅ | ✅ with a paid key on macOS | Out |
| Playwright on the frontend, IPC mocked (the test plan's default) | ❌ | ✅ | ✅ | Already covered by Vitest and Storybook fixtures; misses the corral |
| XCUITest on the bundled app (`XCUIApplication(bundleIdentifier:)`) | ✅ through Accessibility | ✅ partly (web content shows in the AX tree) | ⚠️ needs the Accessibility permission (TCC); its prompt can't be clicked from a test, the session must be unlocked and the app in front | Nightly smoke only |
| Accessibility scripting (System Events, xa11y) | ✅ | ✅ partly | ⚠️ same permission | Out: weaker checks than XCUITest, same cost |
| **Journey transcripts** (this spike) | ✅ real decoding and models, gestures in-process | ✅ Vitest on the same fixtures | ✅ already runs (`cargo test`, `xcodebuild test` on `macos-26`) | **Every PR** |

Why the transcripts are enough to gate on: every byte between the core and the corral is JSON (ADR-001, `bridge.rs`). Rust already decodes the intent fixtures that Swift is tested to encode (TP-BRIDGE-02). A transcript extends that from single messages to whole journeys. Each half is checked against the same recording, so drift on either side fails a test. What it doesn't cover is the running process: threads, the run loop, the Tauri shell starting the panel, and AppKit turning a real click into the model's gesture. That is exactly what the nightly smoke covers.

## How the transcripts work

1. **The driver** (`apps/desktop/src-tauri/tests/journeys.rs`): builds the real core with an in-memory store and the journey's `FakeAdapter` scenario, then runs the journey's steps. A step is a user intent, decoded from the same JSON Swift sends, or a main-window command (`save_agent`, say), called as the Tauri command would. The driver records what the core sends back: the roster, notifications, the prompt bar's feed and Touch ID requests. The test fails if the checked-in transcript differs, so a change to the core shows up as a reviewed diff, like `story_fixtures.rs` does for Storybook.
2. **The transcript** (`native/CorralNative/Fixtures/journeys/J9.json`, one per journey, beside the bridge fixtures): ordered steps, each with its gesture (`click drop scout`, `hover`, `press Deny`, `⌘-click`), the intent that gesture must produce, and the payloads that follow. Timestamps and IDs come from the fake clock and fixed seeds, so the file is stable.
3. **The replay** (`CorralJourneyTests.swift`): decodes each payload as the bridge does (`CorralRoster.decode` and its siblings), builds the corral's models from it (the layout, `CorralCardModel`, the prompt bar's), performs each gesture on them as `CorralKeyboardTests`, `CorralHoverTests` and `CorralApprovalTests` do, and checks the emitted intent with `sameJSON`. It then checks the step's expectations on the view model, plus a render snapshot at the journey's key frames.
4. **Main-window steps** (J1's key and agent, J4's viewer, J5's cap) run in Vitest against the app with fakes fed from the same core-generated fixtures, as the stories are.

Rule 4 holds: no gate bypass. Every side effect in a journey goes through `ToolGateway` with a `Decision::Allow`, and checkpoints are real ones from the policy.

## The journeys, by layer

| Journey | Every PR: transcript (corral) | Every PR: Vitest (main window) | Nightly native |
|---|---|---|---|
| J1 first launch → key → agent → deploy → done → send back | deploy, column, card, done, send back | add the key, create the agent | ✅ |
| J2 deploy three at once | ⌘-click ×3, "Deploy to 3", three columns | – | – |
| J3 checkpoint: approve, deny with note, edit args, allow for run | all four, Touch ID through its payload | – | – |
| J4 trace from a task card, filter, copy | the open-trace intent | filter to tool calls, copy JSON | – |
| J5 soft limit → checkpoint → raise → continue | checkpoint, then continue | raise the cap | – |
| J6 recall all from the hotkey, 5 runs | stop-all, five rings stop | – | – |
| J9 corral morph | ✅ | – | – |
| J10 usage order and shown set | ✅ (idle-gated reorder, the 9th agent's chooser) | the chooser in Settings → Corral | – |
| J11 hover approval | ✅ | – | – |
| J0 walking skeleton | (already TP-SKEL-03..05) | – | ✅ |

J7 (voice), J8 (manifest import) and J12 (Compare) aren't in M4's exit; they join when their milestones do.

## Performance budgets

The nightly native job measures them on the bundled app with `os_signpost` intervals: summon to interactive, event emitted to card rendered, idle frames, cold start, memory with eight runs. Each budget fails the job when it's missed three nights running, to absorb runner noise. Two can be checked on every PR without the app: the morph's GPU time per frame, as an `XCTMetric` in `just native-test`, and the trace viewer's first paint for a 5,000-event run, in Vitest with the core's fixture. Hosted runners are slower and noisier than an M1, so the budgets are checked against a calibration run on the same runner. The absolute numbers are confirmed by hand on Anton's Mac before release.

## Order of work

1. ✅ **J9 end to end:** the driver, the transcript format and the replay, for the journey with no main window and no checkpoint (2026-11-26). The driver plays through `CorralState`, the state the app's feed holds, now built on every platform, so the transcript is what the app sends.
2. ✅ **J2, J3, J11:** deploys and checkpoints (2026-11-27). The prompt bar's glue and the checks on a card's answer (`waits_on`, `edited_args`) are now shared with the app too, so the driver answers checkpoints on the desk as the app does.
3. ✅ **J1, J4, J5, J6, J10:** the journeys that cross into the main window. J6 and J10 done (2026-11-27): a transcript step done in the main window records its command and the core's answer, and `apps/desktop/src/journeys.test.tsx` plays it on the real screen. J1 and J5 done (2026-11-27): the driver runs with the app around the core (the keys, the agent editor, the logos folder) in a scratch home recorded as `/Users/you`. J4 done the same day: the driver logs to the app's event store, which the trace viewer reads.
4. **The nightly:** one proof run of XCUITest on `macos-26` (the permission question), then J0, J1 and the performance job. This needs a new workflow under `.github/`, a protected path, so it's its own PR for review. The proof run's workflow is in (2026-11-27): `.github/workflows/ui-nightly.yml` turns off UI automation's password prompt (`automationmodetool enable-automationmode-without-authentication`, Apple's answer for CI machines), generates `native/CorralUIProof` with a pinned, checksummed XcodeGen, and clicks a button in a window and in a non-activating panel. Its first run answers the permission question.

Steps 1–3 need no CI change: they run in the existing `cargo test` and native jobs.

## Still open

- Whether GitHub's hosted `macos-26` image grants Accessibility to the test runner. Apple's forums show hosted runners blocked by TCC prompts; CircleCI documents writing the permission into TCC's database before tests. Step 4's proof run answers it. If it can't, the nightly moves to a self-hosted Mac mini.
- The `e2e` feature must never reach a release build. A CI check on the release binary (no `FakeAdapter` symbols) goes with step 4.

## Sources (checked 2026-11-25)

- Tauri, WebDriver testing: https://v2.tauri.app/develop/tests/webdriver/
- WebdriverIO, Tauri platform support: https://webdriver.io/docs/desktop-testing/tauri/platform-support
- Apple, `XCUIApplication.init(bundleIdentifier:)`: https://developer.apple.com/documentation/xcuiautomation/xcuiapplication/init(bundleidentifier:)
- Apple Developer Forums, UI tests on a locked CI session: https://developer.apple.com/forums/thread/840965
- Apple Developer Forums, UI tests blocked by a permission prompt on GitHub Actions: https://developer.apple.com/forums/thread/806451
- CircleCI, testing macOS apps (UI-test permissions): https://circleci.com/docs/testing-macos
