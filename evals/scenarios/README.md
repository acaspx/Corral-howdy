# Scenarios

Cost, control and UX scenarios (`EV-COST-*`, `EV-STOP-*`, `EV-UX-*`) live here.
Safety scenarios (`EV-SAFE-*`) live in [`../safety/`](../safety/), a protected path that needs the code owner's review.

The runner loads both folders recursively and validates every file against a strict schema
(`crates/corral-evals`): unknown keys anywhere are errors, so a typo can't quietly drop an expectation.
