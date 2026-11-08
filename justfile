# Corral task runner. `just --list` to see everything.
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Everything CI runs on a PR. Run before every push.
check: lint test (eval "gate") deny

# Formatting, lints and type checks (Rust + TS). Builds the frontend first: the Tauri shell embeds it.
lint:
    cargo fmt --all -- --check
    pnpm install --frozen-lockfile
    pnpm -r typecheck
    pnpm -r lint
    pnpm -r build
    cargo clippy --workspace --all-targets -- -D warnings

# Rust and TypeScript tests.
test:
    cargo test
    pnpm -r test

# Run an eval suite: gate | full  (docs/05-evals.md)
eval suite="gate":
    cargo run -q -p corral-evals -- --suite {{suite}} --report evals/reports/{{suite}}.json --baseline evals/reports/baselines/main.json

# Dependency advisories, licenses and sources (needs cargo-deny).
deny:
    cargo deny --workspace check

# Run the app with hot reload.
dev:
    pnpm install
    pnpm --filter @corral/desktop tauri dev

# Swift tests for the corral surface (macOS 26 + Xcode 26).
native-test:
    cd native/CorralNative && xcodebuild test -scheme CorralNative -destination 'platform=macOS'

# UI components with fixture event streams (lands in M4).
storybook:
    @echo "Storybook arrives with the main-window components in M4 (docs/03-build-plan.md)."
