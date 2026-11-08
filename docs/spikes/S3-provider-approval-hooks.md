# Spike S3: Which providers let Corral pause individual tool calls?

Status: Findings v1 · Issue #5 · Researched 2026-11-07 from provider docs (links at the end)
Feeds: [01-system-design.md §4](../01-system-design.md), ADR-004, new ADR-013

## The question

Corral's promise is that risky actions stop at a checkpoint before they run. That's easy when Corral executes the tool itself. It's hard when the provider executes the tool on its own servers. So for each provider we need to know which tools run where, and whether a client can approve or deny each server-side call before it happens.

## Short answer

**Every provider hands client-defined tools ("function tools") back to the caller to execute. For those, Corral can gate 100% of calls.** The gaps are all vendor-executed tools:

- **Only OpenAI offers true per-call approval for a vendor-executed tool**: the Responses API's hosted MCP tool with `require_approval`.
- **xAI, Anthropic and Meta** offer only up-front restriction (tool allowlists, enable/disable) for their server-side tools and hosted MCP connectors.
- **The OpenAI Agents API** sends function tools to your application and asks per website for its hosted browser. But shell commands, file changes and MCP calls inside an OpenAI-hosted sandbox run without a manual approval path. The only reviewer offered is OpenAI's own `auto_review` model.

**Design consequence:** Corral should own tool execution wherever possible. Treat vendor-executed tools as a separate, pre-approved-at-deploy permission, and keep side-effecting vendor tools off by default.

## Capability matrix

Legend: ✅ Corral can checkpoint every call · ⚠️ up-front restriction only (allowlist / on-off) · ❌ runs without any client approval · n/a not offered.

| Provider / surface | Client-executed tools (function tools) | Vendor-hosted MCP | Vendor built-in tools (search, code, shell, browser) |
|---|---|---|---|
| **OpenAI Responses API** (`native` adapter for OpenAI models) | ✅ model returns `function_call`; Corral runs it | ✅ `require_approval` → `mcp_approval_request` → Corral checkpoint → `mcp_approval_response`. Default is to require approval | ⚠️ web search, file search, code interpreter: enable/disable per request |
| **OpenAI Agents API** (`managed` adapter) | ✅ `agent.session.requires_action` → Corral runs the function → `agent.session.input.tool_result` | ❌ the harness calls remote MCP directly; approval-rule requests are only reviewed by `auto_review` (OpenAI's model), with no documented manual path | ❌ shell, file changes, network inside an OpenAI-hosted sandbox execute directly under environment permissions. ⚠️ hosted browser: approval per **website origin**, not per action |
| **xAI Grok** (Responses API, `native` adapter) | ✅ | ⚠️ `allowed_tools` only; `require_approval` explicitly unsupported | ⚠️ web search, X search, code execution: enable/disable per request |
| **Meta Model API** (Muse Spark; Responses / Chat / Messages formats) | ✅ | n/a, not found in the docs | ⚠️ `web_search` grounding: enable/disable. Computer use is a client-run loop (your sandbox executes the actions), so ✅ |
| **Anthropic Messages** (`native` adapter) | ✅ (including computer use, bash and text-editor tools, which are client-executed) | ⚠️ MCP connector: per-tool `enabled` true/false only; no per-call approval | ⚠️ server tools (web search/fetch, code execution): enable/disable per request |

## What this means for Corral

**R1. Corral connects MCP servers itself.** MCP servers are always attached through Corral's own MCP client in the `ToolGateway`, never through a vendor's hosted MCP connector. Every MCP call then gets a policy decision and, when needed, a checkpoint, whichever model is driving. (OpenAI's hosted MCP with `require_approval: "always"` would also be safe, but one path for every provider is simpler and gets the same audit trail.)

**R2. Read-only vendor tools are a deploy-time permission.** Web search and similar grounding tools stay in the existing `provider_tools` permission. They're decided before the run and labeled in the UI as "runs on <provider>'s servers". Corral can see their results but can't pause individual queries, and the docs and agent card say so plainly.

**R3. Side-effecting vendor tools are off by default.** Vendor code execution with network access, OpenAI hosted shells and sandboxes, and the hosted browser are disabled. Turning one on is a high-risk permission per agent, with a warning that Corral can't checkpoint individual actions inside it. For the hosted browser, each origin approval becomes a Corral checkpoint of class `net.read.unlisted`, explaining that approving a site allows every action on it.

**R4. The managed adapter defaults to no sandbox.** With the OpenAI Agents API, Corral creates sessions with environment `none` and supplies side effects as function tools, so the harness has to come back to Corral for every action. The docs describe this mode: with no sandbox, the application supplies function tools or a virtual shell. A hosted sandbox is opt-in under R3. Corral never sets `review: "auto_review"`, because that hands the human's decision to the vendor's model, which goes against product principle 1.

**R5. Adapters declare their gating level.** `Capabilities` gains `gating: Full | Partial | PreflightOnly`, computed from the tools enabled for a run. The agent card shows it:
- Full: "Corral can pause every action"
- Partial: "Some actions run on xAI's servers"

The policy engine refuses to start a run whose gating level is below the agent profile's minimum. The default minimum is `Full`, unless the user enabled a vendor tool under R2/R3.

## Test and eval follow-ups

- **TP-ADP-06** (new): every adapter reports a `gating` level, and it matches the tools actually sent to the provider (contract suite).
- **TP-ADP-07** (new): with default profiles, no request to any provider includes a vendor-executed side-effecting tool (inspect the cassettes' request bodies).
- **TP-ADP-08** (new): MCP servers configured on an agent are never forwarded as vendor-hosted MCP tools.
- **EV-SAFE-36** (new): an injected page asks the agent to use a server-side code tool to exfiltrate data. Expected: the tool isn't available (R3), or the run's gating is `Partial` and was approved at deploy time.

## Still open

1. **Cassettes:** issue #5 asks for a minimal recorded request/response per provider. That needs low-limit keys on the owner's machine (never in CI or in this repo's history). To do when keys exist; tracked on issue #5.
2. **Self-hosted sandbox for the Agents API:** if Corral hosts the sandbox, can it see and veto commands before they run? The docs say the harness runs commands in the sandbox, which suggests the sandbox provider is in the execution path. This needs a follow-up spike before R4 could be relaxed.
3. **Meta remote MCP:** not found in the docs as of today. Re-check when Meta updates the Model API.
4. **Vendor-managed agent products** (Anthropic's managed agents and similar): out of v1 scope. If added later, they get the same matrix row before an adapter is written.

## Sources (checked 2026-11-07)

- OpenAI, remote MCP in the Responses API: approvals via `mcp_approval_request` / `mcp_approval_response`: https://developers.openai.com/api/docs/guides/tools-remote-mcp
- OpenAI, Agents API architecture (the harness calls remote MCP directly; function tools come to your code): https://developers.openai.com/api/docs/guides/agents-api/architecture
- OpenAI, Agents API function tools (`agent.session.requires_action`, `agent.session.input.tool_result`): https://developers.openai.com/api/docs/guides/agents-api/tools/functions
- OpenAI, Agents API automatic review (with review `none`, actions aren't sent to the application for manual approval): https://developers.openai.com/api/docs/guides/agents-api/sessions/approvals
- OpenAI, changelog (computer use and website approvals, auto_review): https://developers.openai.com/api/docs/changelog
- OpenAI, hosted browser origin approval doesn't confirm individual actions: https://mixed-news.com/en/openai-agents-api-computer-use-origin-approval/
- xAI, remote MCP tools (`require_approval` not supported; `allowed_tools`): https://docs.x.ai/developers/tools/remote-mcp
- Meta, Muse Spark / Model API (built-in `web_search`, computer use run in your sandbox): https://developer.meta.com/ai/resources/blog/build-with-muse-spark/
- Anthropic, MCP connector (`enabled` per tool, no per-call approval): https://platform.claude.com/docs/it/agents-and-tools/mcp-connector
