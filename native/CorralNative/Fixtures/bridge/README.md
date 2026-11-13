# Bridge fixtures (ADR-001)

The wire contract between the Rust core and CorralNative over the C ABI (docs/01 §2).
Both sides test against these files, so a change on one side fails the other side's tests.

Rust → Swift:
- `roster-v1.json` (`corral_native_set_roster`): at most 8 agents, most used first.
- `roster-cards-v1.json`: the same roster with run cards (SK-6): what each drop's hover card and
  result card show, from the core's projections. Agent text in them is already cleaned and capped;
  Swift bounds it again.
- `roster-logos-v1.json`: a drop's provider logo, as a path into Corral's `logos` folder
  (ADR-015). Swift checks the path and the file again.
- `roster-linked-v1.json`: linked apps (consumer Muse and your dot, ADR-005). A linked drop
  carries the app it opens in (`linked`), and `links` says which linked apps are shown, for the
  corral's right-click menu.
- `notify-v1.json` (`corral_native_notify`): a checkpoint notification. Rust chooses the buttons
  (high and critical risks get no inline Approve); Swift only draws them.
- `authenticate-v1.json` (`corral_native_authenticate`): a Touch ID prompt. Rust asks; Swift reports.

Swift → Rust, through the intent callback:
- `intent-*-v1.json`: what the user did (a tap, a notification button, a Touch ID result). SK-6
  adds a task deployed from a drop (`deployTask`), Stop, Send back and Retry.
  Linked apps add `showLinked`, `hideLinked` (from the right-click menu) and `openLinked` (the
  card's Open button). Rust checks the app and the look, saves the choice and opens the app.

TP-BRIDGE-01..04, TP-SKEL-03..05, TP-LOGO-09 and TP-LINK-03..05 (docs/04-test-plan.md).
