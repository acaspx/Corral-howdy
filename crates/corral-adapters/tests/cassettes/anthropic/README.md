# Anthropic cassettes

Hand-written exchanges with Anthropic's Messages API (`POST /v1/messages`, streamed), replayed by
`CassetteTransport` so tests never call the network (AGENTS.md rule 7). The format is the same as
`../xai/`: each response's HTTP status, an optional `Retry-After`, and the body line by line.

They follow Anthropic's streaming docs: `message_start` carries input usage, each content block
is `content_block_start`, its deltas and `content_block_stop`, then `message_delta` (stop reason
and cumulative output usage) and `message_stop`; `ping` and `error` can arrive at any point.
Thinking signatures are replaced with `<redacted>`.

Request headers are never stored, so a key can't end up here. Tests check what Corral sends (the
`x-api-key` header name, `anthropic-version`, and a body with no tools) against the transport's
in-memory record instead.
