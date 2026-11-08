# Corral: Product Brief

Status: Draft v0.1 · Owner: Anton Castro · Reviewer: CJ Pais · Last updated: 2026-11-07

## One line

Corral is a macOS desktop harness where you keep all your AI agents in one place: a liquid-glass corral at the bottom of your screen, whose shape shows how many agents are home. From there you deploy them on tasks by text or voice, watch them work live, approve or deny their risky actions, inspect their traces, and send them back to the corral when they're done.

## Problem

People who use several agents (Grok, Meta's Muse, OpenAI's dots, Claude Code, their own custom agents) end up juggling terminals, browser tabs, vendor apps and API dashboards. Each one has its own approval model, cost meter and history, or none at all. Nothing gives a single answer to "what are my agents doing right now, what do they want from me, and what has it cost?"

## Who it is for

| Release | User | What they need |
|---|---|---|
| v1 (developers first) | Developers and technical power users on **macOS 26 or later** who already hold API keys and run CLI agents | Bring your own keys (BYOK), config files, traces, safe defaults, installs from GitHub/Homebrew |
| v2 (consumers next) | Non-technical people with agent subscriptions | Signed DMG, guided onboarding, no terminal, plain-language checkpoints |

## Core jobs

1. **Keep a roster.** See every agent you've configured, its provider, status and spend. The corral's shape is the headcount: one drop is a circle, two a dumbbell, three a triangle, four a square, and so on up to an octagon. With more than 8 agents, you choose which ones show. Your most-used agent sits top-left and the rest follow clockwise, re-ordering as your usage changes (see [07-brand-and-corral-surface.md](07-brand-and-corral-surface.md)).
2. **Deploy.** Click an agent (or several, or all), give it a task by text or voice, and it leaves the corral.
3. **Keep tabs.** Each deployed agent gets a live activity card showing the current step, tool in use, elapsed time and spend.
4. **Decide.** Risky actions pause at a checkpoint. You approve, deny, edit or "allow for this run".
5. **Inspect.** Open a full trace: every model call, tool call, checkpoint and result, in order.
6. **Recall or re-task.** From the activity card, send the agent back to the corral or prompt it again with its context kept.

## What "agents like Grok, Muse, Dot" means in practice (Nov 2026)

This drives the adapter design, so it is spelled out here (sources in the system design):

- **Grok (xAI):** The public API is OpenAI-compatible, with a Responses API and server-side tools such as web search and code execution. Corral can run Grok agents directly with the user's xAI key.
- **Muse (Meta):** Muse is a consumer agent that runs on its own Meta-hosted VM, with Meta's own approval layer. No official API lets a third party drive it. Developers *can* reach the same model family (Muse Spark) through the Meta Model API and Muse Code, so Corral can run **Muse Spark-powered agents** but cannot drive a user's consumer Muse.
- **Dots (OpenAI):** Dots live inside ChatGPT and have no dots API endpoint. The developer path is OpenAI's Agents API (public beta). Corral can run **Agents API agents** but cannot drive a user's consumer dot.
- **CLI agents** (Claude Code, Codex CLI, Gemini CLI, and others) speak the Agent Client Protocol (ACP) over stdio. Corral can host them as a client.
- **Custom agents:** the user defines a persona, model, tools and permissions in a manifest, and Corral runs it.

Product rule: **Corral never automates a consumer agent through unofficial wrappers, scraped sessions or shared passwords.** When Meta or OpenAI publish an official control API, it becomes another adapter. Until then, consumer Muse and dots appear as "linked apps" (an open-in-app shortcut plus a manual status), clearly marked as not managed.

## Principles

1. **The human is the circuit breaker.** Anything that leaves the Mac, changes files or spends money past a threshold asks first, unless the user has explicitly widened that agent's policy.
2. **Local first.** Keys live in the macOS Keychain and history in a local database. Sync is opt-in and end-to-end encrypted.
3. **One mental model for every agent.** Whatever the provider, every agent has the same lifecycle, the same activity card and the same trace format.
4. **Calm by default.** Checkpoints must be rare enough that people actually read them. Approval fatigue is a safety bug.
5. **Test-first and eval-gated.** No behavior ships without a failing test that turned green. No safety-relevant change ships without the safety eval suite passing.
6. **Forkable.** In the spirit of Handy: clean crates, clear docs and a codebase a contributor can understand in an afternoon.

## v1 scope

In:
- The liquid-glass corral at the bottom center of the desktop (Swift + Metal shader), glass activity and checkpoint cards, a menu-bar fallback, and a main window (traces, settings)
- Native API agents for xAI Grok, OpenAI-compatible endpoints, Anthropic, and the Meta Model API (Muse Spark)
- OpenAI Agents API adapter
- ACP adapter for local CLI agents
- Custom agent manifests (`corral-agent.toml`)
- Per-agent permission profiles, checkpoints, budgets, Recall-all kill switch
- Push-to-talk voice prompting (on-device transcription)
- Trace viewer, with OpenTelemetry GenAI–style export
- Signed, notarized release, Homebrew cask, auto-update

Out of v1 (planned later):
- Optional end-to-end encrypted sync across Macs (v1.1)
- A2A remote agents (v1.1)
- WidgetKit desktop widget (spike in v1, ship in v1.x)
- Team/shared corrals, consumer onboarding polish (v2)
- Windows/Linux builds (Tauri makes these possible; not a goal now)

## Success measures for v1

| Measure | Target |
|---|---|
| Time from install to first deployed agent | < 3 minutes with an API key in hand |
| Checkpoint recall on the safety eval set | 100% |
| Checkpoint precision (share of checkpoints that were real risk) | ≥ 90% |
| Recall-all: every run halted | p95 < 2 s |
| Spend shown vs provider-reported | within 2% |
| Corral summon latency (hotkey → prompt bar) | p95 < 150 ms |
| Idle CPU with 10 agents in the corral | < 1% (an idle corral renders no frames) |

## Open questions

1. ~~**License**~~ Decided: **MIT** for the code (matches Handy); the Corral name, logo and liquid-glass visual identity are reserved, not open source.
2. **Shell tool in v1:** offer a sandboxed shell tool to native agents, or leave shell work to ACP CLI agents, which bring their own? Recommendation: CLI agents only in v1.
3. **Telemetry:** none by default. Is opt-in crash reporting acceptable?
4. **Name collision check** on "Corral" for the bundle ID, Homebrew cask and trademark.
5. **Consumer agent APIs:** who watches for official Muse and dots APIs, and how often?
