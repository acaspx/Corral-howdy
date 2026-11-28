# Corral UI proof (Spike S2, step 4)

A tiny app and its XCUITest, to answer one question before the nightly drives the bundled Corral
app: can a GitHub-hosted `macos-26` runner run a macOS UI test? The app has a window and a
floating, non-activating panel (like the corral), each with a button the tests click.

- **Where it runs:** `.github/workflows/ui-nightly.yml`, nightly and on demand, never on a pull
  request. It allows UI automation without a password prompt (`automationmodetool`), generates
  the Xcode project from `project.yml` with a pinned, checksummed XcodeGen, and runs the tests.
  The result bundle is kept as an artifact.
- **On your Mac:** `xcodegen generate` here (`brew install xcodegen`), then open
  `CorralUIProof.xcodeproj` and run the tests (⌘U). The first run asks to allow UI automation.
- **What next:** if the proof passes on the hosted runner, the nightly grows into J0 and J1 on the
  bundled app (built with an `e2e` feature that swaps providers for `FakeAdapter`) and the
  performance budgets. If it can't, it moves to a self-hosted Mac.
