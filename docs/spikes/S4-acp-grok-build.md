# Spike S4: Hosting a CLI agent over ACP, starting with Grok Build

Status: Findings v1 for Grok Build · Issue #6 · Researched 2026-11-25 from xAI's docs and the
`xai-org/grok-build` source at `2bdd1d6` (upstream `559751f`) · Claude Code, Codex CLI, Gemini CLI
and Devin to follow
Feeds: ADR-017, [01-system-design.md §4](../01-system-design.md), [02-security-model.md](../02-security-model.md) T22, T23, T31, T32

## The question

ADR-017 lets a SuperGrok or X Premium+ subscriber run Grok in Corral by hosting xAI's own agent,
Grok Build, over the Agent Client Protocol. Corral is the ACP *client*; Grok Build runs the agent
loop and its tools. What can Corral see and stop, and what does it have to say it can't?

## Short answer

**Most of what matters goes through Corral's gate. Reads by search and listing, and xAI's web
tools, don't.**

- **Corral can serve file I/O and commands, and Grok Build uses them.** If Corral advertises
  `fs.readTextFile`, `fs.writeTextFile` and `terminal` at `initialize`, Grok Build sends file
  reads and writes, and every shell command, to Corral instead of touching the disk itself. Each
  becomes a `ToolGateway` call: `fs.read`, `fs.write` and `exec.shell` against the agent's policy
  and sandbox roots. ACP has no delete, so a delete has to be a shell command, which is gated
  too.
- **Ask mode is set on the command line.** `grok --no-auto-update --permission-mode ask agent
  stdio`: the command-line mode beats `[ui] permission_mode` in the user's config, and an unknown
  mode falls back to ask. Grok Build advertises no ACP session modes, so the command line is the
  only lever.
- **What Corral can't see:**
  - Grok Build's `grep` (ripgrep) and `list_dir` walk the disk directly, even when Corral
    serves files.
  - Its web search and fetch run on xAI's side.
  - Allow rules merged from `~/.grok/config.toml`, a trusted project's `.grok/config.toml`,
    managed settings and a project's `.claude/settings.json` can pre-approve a tool, so Grok
    never asks.

  Gating is `Partial`, and the deploy sheet says exactly this.

## What the source shows

| Topic | Where (`crates/codegen/…`) | Finding |
|---|---|---|
| Client file system | `xai-grok-workspace/src/file_system/adapter.rs`, `xai-grok-shell/src/session/acp_session_impl/spawn.rs` | With the client's fs capabilities and a gateway, the session's file system is `AcpFsAdapter`: `read_file` → `fs/read_text_file`, `write_file` → `fs/write_text_file`, `delete_file` → error |
| Client terminal | `xai-grok-shell-terminal/src/adapter.rs`, `spawn.rs` (`select_terminal_backend_kind`) | With the client's `terminal` capability, bash runs through `terminal/create`, `terminal/output`, `terminal/wait_for_exit` and `terminal/release`. The request carries the exact command, the environment and the working directory, but no tool-call ID. A subagent reuses its parent's backend |
| Search and listing | `xai-grok-tools/src/implementations/grok_build/grep/ripgrep.rs`, `…/list_dir` | Local; not routed through the client |
| Permission modes | `xai-grok-telemetry/src/enums.rs`, `xai-grok-shell/src/util/config/permissions.rs` | `Ask` (default), `AlwaysApprove`, `Auto` (an LLM classifier reviews calls). The CLI flag beats the config; unknown strings mean `Ask` |
| What asks | `xai-grok-tools/src/permission_access.rs` | Read, list, grep and web search count as reads; every other tool prompts as an edit, a command, an MCP call or a fetch, so a new tool can't run unasked |
| Allow rules | `xai-grok-permission-rules/src/resolution.rs` | Rules merge from the global config, trusted project configs, managed or enterprise settings, and `.claude/settings.json`. An untrusted clone's project rules are ignored |
| Prompting | `xai-grok-workspace/src/permission/prompter.rs` | Asks through `session/request_permission` |
| Sign-in | `xai-grok-login` | `grok login` (browser OIDC) stores `~/.grok/auth.json`. Corral never reads it |

## How Corral maps it

1. **Launch.** Corral launches the pinned `grok` (path and SHA-256, asking again when either
   changes, T23) as `grok --no-auto-update --permission-mode ask agent stdio`, under the reaper
   (T26), with the scrubbed environment (T14). It has no `XAI_API_KEY`: the subscription is the
   point.
2. **Initialize.** Corral advertises `fs.readTextFile`, `fs.writeTextFile` and `terminal`. It
   passes no `mcpServers`; Corral's own MCP client is the only one (S3, R1).
3. **A permission request is a checkpoint.** `session/request_permission` carries Grok Build's
   description of the tool call (kind, title, locations). Corral decides it with the agent's
   policy, asking the user when the policy says ask. It answers `allow_once` or `reject_once`,
   never an "always" option: allow-for-run is Corral's, scoped as T21 says.
4. **The effect is gated again, exactly.** The `fs/write_text_file` or `terminal/create` that
   follows goes through `ToolGateway`. An approval from step 3 covers only the path or command it
   showed. Anything different (another path, an edited command) asks again. A request with no
   approval is decided on its own.
5. **Cancel.** Stop sends `session/cancel`, then closes stdin, waits up to 2 s and kills the
   process group (EV-STOP).
6. **Spend.** No dollars. The card shows the plan's usage if Grok Build reports it. One client
   uses an `_x.ai/billing` ACP extension for that; it is undocumented, so it's to be verified.

## Still open

- **The real message shapes.** The toy client has to run against a signed-in `grok` on the
  owner's Mac. It should record `initialize`, a permission request per tool kind, the fs and
  terminal calls, cancel, and a usage-limit error, as scrubbed cassettes for the contract suite.
- **Whether Ask mode asks for reads.** The source counts reads separately; the toy run will show
  whether `Ask` prompts for them or lets them through.
- **Allow rules the user already has.** Options: refuse to start when Grok's config has allow
  rules; start, and list them on the deploy sheet; or launch with an empty config home. The last
  would hide the sign-in too, since `auth.json` lives there, unless Grok Build gains a flag for
  it. The leaning is to start and list them.
- **Not signed in.** Corral can say "Sign in to Grok: run `grok login`", or open Terminal with it.
  It never handles the credentials either way.
- **The rest of S4:** Claude Code, Codex CLI, Gemini CLI and Devin, as the build plan lists.

## Sources (checked 2026-11-25)

- xAI, Grok Build headless and ACP: https://docs.x.ai/build/cli/headless-scripting
- `xai-org/grok-build` (Apache-2.0): https://github.com/xai-org/grok-build
- AI SDK, ACP harness notes on Grok Build (no ACP session modes): https://ai-sdk.dev/providers/ai-sdk-harnesses/acp
- An ACP client's Grok driver (launch flags, capabilities): https://github.com/coconiiruzo/acpxx/pull/6
- CodexBar, Grok usage over ACP: https://github.com/steipete/CodexBar/blob/main/docs/grok.md
