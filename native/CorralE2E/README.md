# Corral journeys on the bundled app (Spike S2, step 4)

XCUITest drives the real, bundled Corral app through J0: add an xAI key, click Grok's drop, ask a
question, watch the ring and the hover card count time and spend, read the answer, send Grok back
(`docs/04-test-plan.md` §6, TP-E2E-05). Every PR already checks each journey as a transcript
(`native/CorralNative/Fixtures/journeys/`); this checks the running app around it: the Tauri
shell starting the panel, the run loop, and AppKit turning real clicks into the corral's gestures.

- **The app:** the e2e build, `--features native,e2e` with `apps/desktop/src-tauri/tauri.e2e.conf.json`
  (bundle ID `com.studioacas.corral.e2e`, so it never shares a data folder with a real install).
  Every agent is a scripted fake that answers after 8 seconds; keys stay in memory and any key is
  accepted; nothing reaches the network (`src/e2e.rs`). Each test also launches it in its own
  empty home folder, so it always starts from a first launch.
- **Where it runs:** `.github/workflows/ui-nightly.yml` (the `journeys` job), nightly and on
  demand, never on a pull request. It builds the e2e app, checks that a release build doesn't
  carry it (`scripts/ci/no_e2e_in_release.sh`), registers it with Launch Services, generates this
  project with a pinned, checksummed XcodeGen and runs the tests. The result bundle, with a
  screenshot of any failure, is kept as an artifact.
- **On your Mac:** from the repo root,
  `cd apps/desktop && pnpm tauri build --bundles app --features native,e2e --config src-tauri/tauri.e2e.conf.json`,
  then, back at the repo root, `open target/release/bundle/macos/CorralE2E.app` once so macOS
  knows it (quit it again),
  then `xcodegen generate` here and run the tests in Xcode (⌘U). The first run asks to allow UI
  automation. The e2e app is safe to run beside the real one: it has its own data folder and
  never touches the Keychain.
