# ChatGPT plan cassettes

Hand-written exchanges with OpenAI's Responses API on a ChatGPT plan (Sign in with ChatGPT,
ADR-017), replayed by `CassetteTransport` so tests never call the network (AGENTS.md rule 7).

- `text-ok.json` is the xAI text answer's stream with OpenAI's names and no billed cost: the
  plan route serves the same Responses events.
- `usage-limit-mid-stream.json` opens the stream as `text-ok.json` does, then fails with the
  plan's usage-limit code, which OpenAI's "Models and inference" page says can arrive after
  streaming starts.
- The error files follow the codes and statuses in OpenAI's "Errors and recovery" page for plan
  usage (developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery, checked
  2026-11-25).

Request headers are never stored, so a token can't end up here. Replace these with recordings
once the owner signs in for the live check.
