# Bridge fixtures (Spike S1)

The wire contract between the Rust core and CorralNative over the C ABI (docs/01 §2).
Both sides test against these files, so a change on one side fails the other side's tests.

Rust → Swift:
- `roster-v1.json` (`corral_native_set_roster`): at most 8 agents, most used first.
- `notify-v1.json` (`corral_native_notify`): a checkpoint notification. Rust chooses the buttons
  (high and critical risks get no inline Approve); Swift only draws them.
- `authenticate-v1.json` (`corral_native_authenticate`): a Touch ID prompt. Rust asks; Swift reports.

Swift → Rust, through the intent callback:
- `intent-*-v1.json`: what the user did (a tap, a notification button, a Touch ID result).

TP-BRIDGE-01..03 (docs/04-test-plan.md).
