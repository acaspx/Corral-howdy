# Security Policy

Corral holds API keys and decides what AI agents may do on your behalf, so we treat security reports as the highest priority.

## Reporting a vulnerability

**Please don't open a public issue.** Report privately through [GitHub Security Advisories](https://github.com/acaspx/corral-howdy/security/advisories/new).

Include what you found, how to reproduce it, and what an attacker could do with it. You'll get an acknowledgment within 3 days. We'll keep you updated as we fix it, and we'll credit you in the release notes unless you'd rather stay anonymous.

We follow a 90-day disclosure window: we aim to ship a fix and publish the advisory within 90 days of your report, sooner for anything actively exploited.

## What's in scope

Anything that breaks the promises in [docs/02-security-model.md](docs/02-security-model.md), especially:

- An agent action that runs without the checkpoint it should require
- API keys showing up anywhere outside the macOS Keychain (logs, traces, the database, exports)
- Escaping an agent's sandbox folder
- A web page, file or tool output that gets an agent to act without your approval (prompt injection that defeats the policy engine)
- Tampering with the event log without detection
- Voice approving a high- or critical-risk checkpoint

## Supported versions

Corral is pre-release. Until v1.0, only the `main` branch is supported.
