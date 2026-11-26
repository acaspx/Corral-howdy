# ADR-017: Your subscriptions, through each vendor's own door

Status: **Proposed** 2026-11-25; accepted when the owner merges it. Amends ADR-005: its rule
stays, and the vendors that now offer an official way in get one.

## Context
The product's promise is to manage your agent subscriptions in one place: Grok, Muse, dots and
the rest. Until now Corral could only run agents on API keys, billed per token. ADR-005 kept
consumer agents out ("linked apps") because no vendor offered an official way to drive one. Two
of them now do:

- **xAI.** Grok Build, xAI's open-source coding agent (Apache-2.0, `xai-org/grok-build`), runs
  as an Agent Client Protocol server (`grok agent stdio`) so other apps can host it. It signs in
  with the user's xAI account (SuperGrok or X Premium+), not an API key.
- **OpenAI.** Since 2026-09-29, "Sign in with ChatGPT" lets a Plus or Pro user let an app use
  their plan for AI requests. For open-source and locally run apps it's self-serve. The app gets
  an OAuth token for the public Responses API with function tools allowed, and requests count
  against the plan's Work and Codex usage.

The others haven't changed:

- **Meta.** Muse's developer programme brings services *into* Muse (connectors); nothing lets
  another app drive Muse. Services that drive it on a user's plan are unofficial.
- **Anthropic.** A Claude subscription is for Claude Code and Anthropic's own apps. Third-party
  products must use API keys and may not offer claude.ai login.
- **OpenAI's dots** have no control API.

## Decision
**A provider can be connected two ways: with an API key, or with your subscription where the
vendor offers an official way in.** Corral never extracts a token from another app, never
spoofs a vendor's client, and never uses an unofficial wrapper or scraped session. That is
ADR-005's rule, unchanged.

| Subscription | How Corral uses it | Adapter | Gating |
|---|---|---|---|
| **ChatGPT Plus / Pro** | Sign in with ChatGPT, plan-usage scopes (open-source flow; Corral is MIT and runs locally). Corral's own agent loop calls the Responses API with the user's token | `native` | **Full**: every tool is a Corral function tool, as with a key |
| **SuperGrok / X Premium+** | Corral launches xAI's `grok` (Grok Build), which signs in by itself (`grok login`). Corral never reads its credentials | `acp` | **Partial** (below) |
| Claude Pro / Max | Not offered; API key only | – | – |
| Muse, dots | Linked apps, open-in-app only (ADR-005) | `linked` | None |

- **ChatGPT's tokens are Corral's secrets.** The access and refresh tokens go in a Keychain item
  (`corral.openai-plan`, account `oauth`), are refreshed at call time and are never logged,
  stored elsewhere or shown (AGENTS.md rule 5). Signing out revokes them at OpenAI. The window
  shows the connected account, "Using ChatGPT plan" on the agent, and a Manage usage action that
  opens ChatGPT's usage settings, as OpenAI's guidelines ask. The user can still connect an
  OpenAI key instead.
- **Grok Build runs under Corral's controls, with its gaps said plainly** (Spike S4):
  - **Flags:** Corral launches `grok --no-auto-update --permission-mode ask agent stdio`. The
    command-line mode beats the user's own config.
  - **The binary:** it is pinned by path and SHA-256, so a changed binary asks again (T23).
  - **Files and commands go through the gate.** Corral offers to serve the agent's file reads,
    file writes and terminal. Grok Build then sends those through Corral, so they pass the
    `ToolGateway` like any tool. Its permission requests become checkpoints.
  - **Gaps:** its search and directory listing still read the disk directly, its web tools run on
    xAI's side, and allow rules in the user's or the project's Grok config can pre-approve a
    tool (T22). The deploy sheet and the agent card list these.
- **Spend becomes usage.** A subscription has no per-token price, so dollar caps don't apply to
  these agents. Their card shows the plan's usage where the vendor reports it. The run limits
  that aren't money (turns, time, the loop limit) still apply. A usage-limit error stops the run
  with the vendor's own action (Manage usage for ChatGPT).
- **The list is reviewed** whenever a vendor announces an official way in, and at least every
  quarter (product brief, open question 5). Muse and dots move from linked to managed the day an
  official control API exists.

## Consequences
- **New adapter work in M5.** ChatGPT plan usage is a small change to the Responses client: the
  plan's route takes no `max_output_tokens` and wants `input` as an array. It also needs the
  sign-in flow (loopback redirect, PKCE) in the shell and the Keychain item. Grok Build is the
  first `acp` agent, ahead of Claude Code, Codex CLI, Gemini CLI and Devin.
- **New threats:** T31, subscription tokens; T32, an ACP agent reading outside the gate. The
  security model has both.
- **OpenAI's terms for Sign in with ChatGPT apply:** plan use is free to the user, the app uses its
  own name, and OpenAI's names and buttons are used only as authorized. ADR-015's provider logos
  are checked against them before release.

## Rejected
- **Calling xAI's API with the Grok Build login.** That would give Grok full gating in Corral's
  own loop, but it means reading another app's credentials. If xAI publishes an official sign-in
  for third-party apps, Grok moves to `native`.
- **Claude through the Agent SDK on a subscription.** Anthropic's terms forbid it.
- **Muse through an unofficial wrapper.** That is exactly what ADR-005 rules out.

## Sources (checked 2026-11-25)
- xAI, Grok Build headless and ACP: https://docs.x.ai/build/cli/headless-scripting
- `xai-org/grok-build` source (Apache-2.0): https://github.com/xai-org/grok-build
- OpenAI, ChatGPT plan usage in open-source apps: https://developers.openai.com/siwc/token-sharing-open-source
- OpenAI, plan usage preview limitations: https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations
- OpenAI, integrating Sign in with ChatGPT (cookbook): https://developers.openai.com/cookbook/articles/sign-in-with-chatgpt
- OpenAI, Sign in with ChatGPT terms: https://openai.com/policies/sign-in-with-chatgpt-terms/
- Meta opens Muse connectors: https://runtimewire.com/article/meta-opens-muse-connectors-developers
- Anthropic's subscription terms, summarised: https://autonomee.ai/blog/claude-code-terms-of-service-explained/
