# xAI cassettes

Recorded or hand-written exchanges with xAI's Responses API, replayed by `CassetteTransport` so
tests never call the network (AGENTS.md rule 7). Each file lists the response to each request in
order: HTTP status, an optional `Retry-After`, and the body line by line (`stall_after_body` keeps
the stream open forever after the body, to test Stop).

Request headers are never stored, so a key can't end up here. Tests check the request bodies
Corral sends (TP-SKEL-02: no tools) against the transport's in-memory record instead.

- The top-level files are hand-written from xAI's docs and the OpenAI Responses event format.
- `recorded/` holds real exchanges captured by `cargo run -p corral-adapters --example record_xai`
  on a Mac with a key in the Keychain. Reasoning `encrypted_content` is replaced with
  `<redacted>`. The same contract tests run over both sets, which catches drift between the docs
  and what xAI actually sends.
