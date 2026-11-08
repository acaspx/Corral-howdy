# Contributing to Corral

Thanks for helping. Corral is built mostly by coding agents working one issue at a time, with people reviewing every change. Human contributors follow the same loop, which is described in full in [docs/06-loop-engineering.md](docs/06-loop-engineering.md).

## Before you start

1. Read [`AGENTS.md`](AGENTS.md). Its rules apply to people too.
2. Pick an issue labeled `ready`. Each one lists the tests that define "done" and the paths you may change.
3. Want to do something without an issue? Open one first so we can agree on scope. Unrequested large PRs are hard to review and likely to sit.

## The loop

1. **Tests first.** Commit the failing acceptance tests named in the issue before any implementation.
2. **Make them pass** without editing existing test assertions. If an existing test is wrong, explain why under "Test changes" in the PR. CI checks this.
3. **Run `just check`** (formatting, lints, types, tests, eval gate, dependency checks).
4. **Open a PR** with the template filled in.

## Setup

- macOS 26 and Xcode 26 (for the Swift corral)
- Rust stable, Node.js 22+ with pnpm, [`just`](https://github.com/casey/just), and [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)

```bash
pnpm install
just check   # everything CI runs
just dev     # run the app
```

## What we'll gladly take

- Issues from the build plan ([docs/03-build-plan.md](docs/03-build-plan.md))
- New eval scenarios, especially prompt-injection cases (`evals/scenarios/`)
- Fixes for provider wire-format drift, with an updated cassette
- Docs that make something clearer

## What needs a conversation first

- New agent providers or adapters
- Anything in a protected path: the policy engine, filesystem sandbox, budgets, Tauri capabilities, CI, `AGENTS.md`, safety evals. These always need a maintainer's review.
- Changes to the corral's look and motion. The brand is reserved (see the License section of the README).

## Security issues

Never in public. See [SECURITY.md](SECURITY.md).
