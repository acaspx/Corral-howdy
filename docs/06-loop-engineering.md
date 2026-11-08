# Corral: Loop Engineering (How Agents Build Corral)

Status: Draft v0.1 · Last updated: 2026-11-07

Corral is built mostly by coding agents (Claude Code, Codex, others) working issue by issue. Anton Castro owns the project and is the human reviewer of record; other collaborators may be invited to review, but approval rests with the owner. The loop has to make good work the easy path and unsafe work impossible to merge.

## 1. Roles

| Role | Who | Does | Never does |
|---|---|---|---|
| **Planner** | Anton (+ a planning agent) | Writes and splits issues with acceptance test IDs; keeps docs current | Merges unreviewed code |
| **Builder agent** | Claude Code / Codex, one per issue | Writes failing tests → implements → runs `just check` → opens PR | Touches unrelated files; edits tests silently; merges |
| **Reviewer agent** | A *different* agent session from the builder, given only the issue, docs and diff | First-pass review against AGENTS.md + docs; posts findings | Approves its own work; approves protected paths |
| **Human reviewer** | Anton (owner); invited collaborators optional | Final review; required on protected paths (CODEOWNERS) | Skip review on protected paths |
| **CI** | GitHub Actions | Runs every gate; posts eval summary | Gets bypassed |

## 2. The loop

```
 ┌───────────────┐   ready   ┌────────────────────┐
 │ Issue (spec + │──────────►│ Builder agent picks│
 │ test IDs)     │           │ up, own worktree   │
 └───────────────┘           └─────────┬──────────┘
        ▲                              │ 1. commit failing tests (red)
        │                              │ 2. implement until green
        │ rules learned                │ 3. just check (local gate)
        │                              ▼
 ┌──────┴────────┐  findings ┌────────────────────┐
 │ Weekly retro  │◄──────────│ PR + CI gates      │
 │ (BL metrics)  │           │ fmt·lint·types·    │
 └───────────────┘           │ tests·eval gate·   │
        ▲                    │ security scans     │
        │                    └─────────┬──────────┘
        │                              ▼
        │                    ┌────────────────────┐
        │                    │ Reviewer agent     │
        │                    │ (separate session) │
        │                    └─────────┬──────────┘
        │                              ▼
        │                    ┌────────────────────┐
        └────────────────────│ Human review →     │
                             │ squash merge       │
                             └────────────────────┘
```

**Inner loop** (minutes, local): red test → code → `just check` → repeat.
**Outer loop** (hours to days, GitHub): PR → CI → reviewer agent → human → merge.
**Learning loop** (weekly): BL metrics + review findings → new lines in AGENTS.md "Rules learned", new evals, better issue templates.

## 3. Loop controls (guardrails on the builders)

1. **One issue, one branch, one worktree.** Branch name `agent/<issue#>-<slug>`.
2. **Iteration budget:** at most 3 red CI cycles per PR. Then the agent stops, labels the PR `needs-human` and writes what it tried.
3. **Scope fence:** the issue lists allowed paths. CI fails if the diff touches paths outside them (plus tests and fixtures) without a `scope-change` note.
4. **Test-change detector:** CI checks every changed or deleted line of existing test code: test files, and inline `#[cfg(test)]` modules (kept at the end of each file by clippy). Any such change requires a "Test changes" section and a human approval. Adding tests never does.
5. **Protected paths** (security model §Protected) require a code-owner review. Agents may *propose* changes there but can't get them merged alone. This includes the **loop-integrity files** (CI, scripts, lint and toolchain config, the eval runner and its baseline). **Checks judge PRs from the base commit:** CI runs the base commit's scope guard and compares evals against the base commit's baseline, every step runs with pipefail, actions are pinned by SHA and the Rust toolchain is pinned. A PR can't make its own checks easier. The checks have their own tests (`just ci-test`, TP-CI-*).
6. **WIP limit:** at most 4 open agent PRs at once, so review stays faster than generation.
7. **No secrets in agent sessions.** Builder agents work with cassettes and fakes only. Live keys exist only in the protected CI environment.
8. **No merges by agents.** Branch protection: required checks, one human approval, linear history, signed commits.
9. **Docs are the spec.** If code and docs disagree, the PR must update the doc or the code, and say which.

## 4. Writing an issue that agents finish in one loop

Use `.github/ISSUE_TEMPLATE/task.md`. A good issue has:
- **Why** in one or two sentences, linking the doc section
- **Acceptance tests by ID** (from the test plan) plus any new ones, with the expected behavior written out
- **Allowed paths**
- **Out of scope** (what not to do)
- **Size:** if you can't list the tests in under 10 lines, split it

## 5. Prompts and context for builder agents

Builders read, in order: `AGENTS.md` → the issue → linked doc sections → the code. AGENTS.md stays short (under ~200 lines). It holds rules, commands and "Rules learned". Long explanations go in `docs/`.

## 6. Cadence

| When | What |
|---|---|
| Daily | Planner triages; humans clear the review queue first, then new work |
| Weekly | Retro: BL metrics, top review findings → AGENTS.md updates; re-plan next issues |
| Per milestone | Exit criteria check; ADRs accepted or amended; re-estimate |
| Per release | Release checklist; threat-model walkthrough |

## 7. Starting the loop (first week)
1. Create the GitHub repo; push this docs set + AGENTS.md + templates + CI skeleton.
2. Turn on branch protection: require the CI checks and one approval. Because the owner can't approve their own PRs, owner-authored PRs merge with admin bypass; agent-authored PRs get a normal approval.
3. Planner files M0 issues and the four spike issues.
4. First builder run: issue "CI skeleton passes on a no-op PR". This proves the loop before any product code.
