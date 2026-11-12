# ADR-001: Stack, the Swift bridge and the process model

Status: **Accepted** 2026-11-10 · Issue #3 · Spike S1
Spike code: `spike/s1-bridge` (step 1 merged in #40; SK-2a turned it into product code and added
step 2). Supersedes the "confirm after Spike S1" note in [01-system-design.md §2](../01-system-design.md).

## Decision

1. **Stack confirmed:** Tauri 2 + React/TS + Rust core + CorralNative (Swift + Metal), as in
   docs/01 §2. Nothing in S1 argues for changing it.
2. **The bridge is a narrow C ABI carrying versioned JSON**, linked statically with `swift-rs`
   (build only; no runtime dependency). Details below.
3. **Process model: keep the core in the app process for v1** (option A below), with the
   mitigations listed. Revisit if real crash data says otherwise.
4. **Checkpoints are approved in the hover card; notifications point to it.** Low and medium risk
   may also approve inline from a notification. High and critical never can, and critical needs
   Touch ID.
5. **A WidgetKit widget is feasible inside the Tauri bundle** and stays a later item. It needs the
   paid Developer ID signing set up first.

## What S1 proved (on Anton's Mac, macOS 26.5, Xcode with the 26.5 SDK)

| Question | Result | Evidence |
|---|---|---|
| Tauri links the Swift package and shows the non-activating corral panel from Rust | Yes | Step 1: 8/8 checks, 25 s, no crash |
| Rust updates the roster live from a background thread | Yes, via `AppHandle::run_on_main_thread` | Step 1 |
| Bad input from Rust can't crash Swift | Refused with a status code; previous roster kept | Malformed roster, unknown notification button |
| Calls from the wrong thread | Refused (`notMainThread`), nothing happens | Off-main probe |
| Taps come back to Rust as typed intents | Yes, once per click (double clicks were ~200 ms apart, real clicks) | Timestamped log |
| The shader ships in the packaged .app | Yes, from `Contents/Resources/CorralGlass.metallib` | Step 2 |
| Actionable notifications from the packaged .app | Yes: Approve, Deny and Open come back to Rust with the checkpoint's nonce | Step 2: 11/11 checks |
| Touch ID for a critical approval | Yes: Rust asks, Swift prompts (biometrics only), Rust decides on the result | Step 2 |
| WidgetKit extension in the same bundle | Feasible; not built (research below) | |

## The bridge

- **One direction each way.** Rust calls `@_cdecl` entry points: `abi_version`, `start`,
  `set_roster`, `notify`, `authenticate`, `stop`. Swift calls back through one function
  pointer with JSON intent bytes, valid only during the call; Rust copies them.
- **JSON, versioned twice.** `corralNativeABIVersion` (Rust refuses to start on a mismatch) and a
  `v` field in every payload. No Rust or Swift type crosses, so there are no shared headers.
- **Contract tests on both sides.** `native/CorralNative/Fixtures/bridge/*.json` are read by the
  Swift tests and the Rust tests (TP-BRIDGE-01..03). Unknown fields are rejected on both sides, so
  the two can't drift silently.
- **Swift trusts nothing.** Every payload is size-checked before it's copied, IDs are validated
  (a bad ID drops that item; IDs are never rewritten), text is stripped of control and bidi
  characters and truncated, the roster is deduplicated and capped at 8. Every entry point returns a
  status code; none traps.
- **Main thread only, by refusal.** Swift returns `notMainThread` rather than hopping threads, so
  ordering stays Rust's job.
- **Swift reports; Rust decides** (AGENTS.md rule 3). Rust chooses the notification buttons and
  when Touch ID is required, and checks single-use nonces (T18). Swift draws what it's sent.
- **`unsafe` lives in one module** (`apps/desktop/src-tauri/src/native.rs`): the extern calls
  and one `slice::from_raw_parts` on the callback, each with a `SAFETY:` comment. The callback is
  wrapped in `catch_unwind` so a panic never unwinds into Swift. The workspace forbids `unsafe`;
  the desktop crate needs `unsafe_code = "deny"` plus one `#[allow]` on that module. **This ADR is
  the "ADR for unsafe" AGENTS.md asks for**, scoped to that one module.
- **Behind a feature.** `--features native` links Swift. Without it the shell builds and tests on
  Linux as before, so `just check` and the Linux CI jobs are unaffected.

## Pitfalls found (each one is a rule for SK-2)

1. **`ShaderLibrary.bundle(.module)` traps** when SwiftPM's resource bundle isn't where SwiftPM
   expects, which is always the case inside the Tauri app. In-process, that trap kills the Rust
   core too. Fixed with `CorralShaders`: look for the metallib in the app's Resources, then for
   the resource bundle without the generated accessor, else draw plain system glass.
2. **`swift build` (what swift-rs runs) doesn't compile Metal.** The shader is compiled with
   `xcrun metal` / `metallib` and copied into `Contents/Resources` through `bundle.resources`.
3. **Notifications need a real .app on disk.** `UNUserNotificationCenter` traps in a bare binary,
   even one with an embedded Info.plist (Tauri dev builds have one). Swift checks for a `.app`
   bundle path before touching it.
4. **Swift 6 completion closures must be explicitly `@Sendable`.** Otherwise a closure written in
   main-actor code is inferred main-actor isolated and traps when the system calls it on its own
   queue.
5. **Banner style loses checkpoints.** With macOS's default Banners, a checkpoint disappears after
   a few seconds (seen in step 2). `NSUserNotificationAlertStyle = alert` in Info.plist only
   applies the first time macOS sees the app. Setup should offer to switch Corral to Alerts, and
   the hover card stays the primary approval surface.
6. **Frontmost suppression.** While Corral is frontmost, macOS shows a notification only if the
   delegate's `willPresent` asks it to. The delegate is set at `start`.

## Process model (T26)

| Option | Upside | Downside |
|---|---|---|
| **A. Core in the app process** (as built) | One binary; no IPC; Keychain, SQLite and the bridge are direct calls | A UI crash ends the supervisor and every run |
| B. Core in a separate helper (launchd agent or XPC service) | A UI crash doesn't stop runs | IPC for every snapshot and intent; two signed binaries; helper lifecycle, login items and updates; Keychain access from the helper; a bigger attack surface |

**Recommendation: A for v1.** Stopping every run when Corral dies is the safe failure: nothing
keeps acting without a supervisor. The real risk in T26 is processes that outlive Corral, and A
handles that with what's already planned or built:
- Swift never traps on input (S1, TP-BRIDGE-03), and the known trap sources above are removed.
- On the next launch, crash recovery marks every unfinished run `Failed(AppRestarted)` (#39,
  TP-SUP-03).
- ACP and CLI agents start under the small reaper that kills their process group when Corral's
  pipe closes (TP-ACP-07), so nothing is orphaned.

Revisit B if crash reports show UI crashes above roughly 1 in 1,000 sessions, or when long
unattended runs become a core use case.

## WidgetKit (researched, not built)

Tauri doesn't build app extensions, so the widget is its own small Xcode project. A
`beforeBundleCommand` builds and signs the `.appex`, `bundle.macOS.files` copies it into
`Contents/PlugIns/`, and Tauri signs the nested code. Data goes through an App Group container:
Swift writes a snapshot and calls `WidgetCenter.reloadAllTimelines()`. `tauri-plugin-widgets` uses
this pattern; Corral should follow the pattern without taking the dependency, since the bridge
already exists. **Ad-hoc signing shows the widget only locally; distribution needs a real Team ID
with App Groups on both targets.**

## Signing and identity

The Apple Developer Program membership is under Anton's personal account. That's fine for
development. **Choose the publishing identity before the first public build:** the Team ID ends up
in the Keychain access group (stored API keys), the App Group (widget) and the signature. Moving
to an organization account later changes the Team ID, so every user would re-enter their keys and
the widget's container would move. Step 2 ran ad-hoc signed; the build skips notarization until
Developer ID credentials are configured (Keychain or CI secrets only, never the repo).

## Open questions

- **Touch ID unavailable** (no sensor, none enrolled, lockout): Swift reports `unavailable`. The
  policy engine must choose the fallback, probably the login password via
  `.deviceOwnerAuthentication` with a stronger confirmation. Decide in M2 (TP-POL-09).
- ~~**CI for the native build**~~: done in SK-2a. The `native` CI job lints and tests the shell
  with `--features native`, next to the Swift contract tests.

## SK-2: from spike to product

- **SK-2a (done):** the checkpoint notification and Touch ID payloads (TP-BRIDGE-04), the spike
  driver and probe entry point removed, the shell shows an empty corral with `--features native`,
  and the CI job above.
- **SK-2b:** the core's corral view (`corral_core::corral_view`, TP-VIEW-01..05) decides what
  each drop shows from the event log; `WatchedLog` feeds it every event the store accepts, and
  the shell sends each change to Swift on the main thread, in order. Intents go to the core with
  SK-6, which brings the prompt field a deploy needs.
- **Packaging (with SK-6 or the first signed build):** compile the metallib in a
  `beforeBundleCommand` and ship it through `bundle.resources` (pitfall 2).
