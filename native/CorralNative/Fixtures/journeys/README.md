# Journey transcripts (Spike S2)

End-to-end journeys from the test plan (§6), recorded by the Rust core and replayed by
CorralNative. Both sides test against these files, so a change on either side fails a test.

- **Recorded by** `apps/desktop/src-tauri/tests/journeys.rs`: the journey played through the real
  supervisor, the corral's controls and `CorralState` (the state the app's feed holds), with
  `FakeAdapter` scripts and a paused clock. The test fails when the core no longer produces the
  file. Regenerate with `CORRAL_WRITE_FIXTURES=1 cargo test -p corral-desktop --test journeys`,
  and review the diff.
- **Replayed by** `Tests/CorralNativeTests/CorralJourneyTests.swift`: every roster must decode,
  each gesture performed on the corral's own card model must send the recorded intent, and what
  the corral shows must match the step's expectations.

Each file has the journey's ID and title, and its `steps`, in order. A step has:

- `say`: what happens, in words.
- `gesture`: what the user does on the corral, or `null` when the core acts (a key connected, a
  run finishing). `{"kind": "task", "agent", "text"}` types a task into a drop's card;
  `{"kind": "press", "agent", "button"}` presses a card button (`stop`, `pause`, `resume`,
  `sendBack`, `retry`).
- `intent`: the intent the gesture must send, as on the wire (see `../bridge/intent-*-v1.json`).
- `rosters`: every roster the core sent during the step, in order.
- `expect`: what the corral shows after it: its `shape` (`triangle`, `square`, …), its `drops` in
  slot order with each one's status, and the trace tree's `columns`.

- `J9.json`: the corral morphs as agents come and go (triangle, square, triangle) while one works,
  finishes and is sent back.
