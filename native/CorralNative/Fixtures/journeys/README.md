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
- `gesture`: what the user does, or `null` when the core acts (a key connected, a run finishing):
  - `{"kind": "task", "agent", "text"}`: Return in a drop's card field, nothing waiting: a task.
  - `{"kind": "note", "agent", "text"}`: Return in the field while a checkpoint waits: Deny, with
    the text as its note.
  - `{"kind": "press", "agent", "button", "pinned"?}`: a card button (`stop`, `pause`, `resume`,
    `sendBack`, `retry`, `approve`, `deny`, `allowForRun`). `pinned`: the drop was clicked first,
    which a high-risk approval needs.
  - `{"kind": "edit", "agent", "changes"}`: the edit sheet, with `changes` typed in, then Approve
    edited.
  - `{"kind": "openBar", "select"}`: ⌘-click the drops in `select`, then open the prompt bar.
  - `{"kind": "typeInBar", "text"}` and `{"kind": "sendBar", "all"?}`: type into the bar, and press
    its send button (`all`: ⌘↩, to every agent).
  - `{"kind": "pointAt", "agent"}` and `{"kind": "leaveCorral"}`: rest the pointer on a drop until
    its card opens, and leave the corral until the card closes; each reports the corral's idle
    state when it changes.
  - `{"kind": "stopAll"}`: Stop all agents, in the corral's right-click menu.
  - `{"kind": "openTraceCard", "agent"}`: a click on the drop's task card in the trace tree.
- `intent`: the intent the gesture must send, as on the wire (see `../bridge/intent-*-v1.json`).
- `rosters`: every roster the core sent during the step, in order (none when nothing changed).
- `bars`: the prompt bar's answers during the step, if any (see `../bridge/prompt-bar-v1.json`).
- `window`: a step done in the main window: its `gesture`, the `command` it calls with its
  `args`, and the core's `answer`. `apps/desktop/src/journeys.test.tsx` performs it on the real
  screen; the Swift replay only takes its rosters. The gestures: `openCorralSettings`, and `tick`
  or `untick` a box by its agent's `name`; `openKeys`, and `pasteKey` with the provider the key
  `looksLike`; `openAgents`, and `rename` an `agent` to a `name`; `viewerOpens`, `show` a kind of
  span with the `spans` the viewer then lists, and `copyJson`. A step that asks the core nothing
  has a `null` command, and Copy as JSON's `clipboard` records the text copied. A key is never
  recorded: the step records the provider and the status the core answers, which shows only the
  key's last 4.
- `settled`: the step's runs end in no fixed order (Stop all cancels them together), so only its
  last roster is kept.
- `expect`: what shows after it: the corral's `shape` (`triangle`, `square`, …), its `drops` in
  slot order with each one's status, and the trace tree's `columns`; and where a step says so,
  each column's status (`columnStatus`, the wire's colour), the `approval` box on a card (its
  class, risk and text its detail holds), the prompt `bar` (its chips, its button and how the
  last send went), the runs the tree counts but doesn't show (`moreColumns`, "+N more running"),
  whether the corral's menu offers Stop all agents (`stopAllOffered`), the name each drop shows
  (`names`), the drops showing their provider's logo (`logos`), and whether the approval box
  offers Allow for this run (`approval.forRun`, checked by the replay).

- `J9.json`: the corral morphs as agents come and go (triangle, square, triangle) while one works,
  finishes and is sent back.
- `J2.json`: three drops ⌘-clicked and one task from the prompt bar; each column folds away as
  its agent finishes.
- `J3.json`: one run asks four times: approve, deny with a note, edit the call, allow for the run.
- `J11.json`: an email waits on you; deny it with a note, then approve the next on the pinned
  card.
- `J1.json`: first launch with an empty corral; an xAI key in Settings, and Grok drops in with
  its logo; named Ada in the agent editor; a task, and Send back.
- `J4.json`: a click on a working run's task card opens it in the main window's trace viewer;
  show its tool calls; copy its events as JSON.
- `J5.json`: a run at the $0.10 soft limit asks on its card; Approve, and it goes on to finish.
- `J6.json`: one task to five agents with ⌘↩, then Stop all from the corral's menu; each can go
  again.
- `J10.json`: the most used agent glides to the top-left once the corral is idle; a 9th agent,
  and Settings → Corral chooses which 8 show.

J1, J4 and J5 run with the app around the core: the keys, the agent editor's settings and names,
the logos folder and the event store the trace viewer reads live in a scratch home folder, recorded as `/Users/you` (as the bridge fixtures
do), so a transcript is the same on every machine.

Today's catalog has four agents. J6 and J10 add stand-ins (Scout, Atlas, …) to reach five and
nine; the corral treats every agent the same.
