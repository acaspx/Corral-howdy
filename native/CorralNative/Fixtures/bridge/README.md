# Bridge fixtures (Spike S1)

The wire contract between the Rust core and CorralNative over the C ABI (docs/01 §2).
Both sides test against these files, so a change on one side fails the other side's tests.

- `roster-v1.json`: what Rust sends (`corral_native_set_roster`). At most 8 agents, most used first.
- `intent-*-v1.json`: what Swift sends back through the intent callback.

TP-BRIDGE-01..03 (docs/04-test-plan.md).
