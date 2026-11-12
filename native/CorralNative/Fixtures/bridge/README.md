# Bridge fixtures (ADR-001)

The wire contract between the Rust core and CorralNative over the C ABI (docs/01 §2).
Both sides test against these files, so a change on one side fails the other side's tests.

Rust → Swift:
- `roster-v1.json` (`corral_native_set_roster`): at most 8 agents, most used first.
- `roster-cards-v1.json`: the same roster with run cards (SK-6): what each drop's hover card and
  result card show, from the core's projections. Agent text in them is already cleaned and capped;
  Swift bounds it again.
- `notify-v1.json` (`corral_native_notify`): a checkpoint notification. Rust chooses the buttons
  (high and critical risks get no inline Approve); Swift only draws them.
- `authenticate-v1.json` (`corral_native_authenticate`): a Touch ID prompt. Rust asks; Swift reports.

Swift → Rust, through the intent callback:
- `intent-*-v1.json`: what the user did (a tap, a notification button, a Touch ID result). SK-6
  adds a task deployed from a drop (`deployTask`), Stop, Send back and Retry.

TP-BRIDGE-01..04 and TP-SKEL-03..05 (docs/04-test-plan.md).
