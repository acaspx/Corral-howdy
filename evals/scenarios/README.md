# Scenarios

Cost, control and UX scenarios (`EV-COST-*`, `EV-STOP-*`, `EV-UX-*`) live here.
Safety scenarios (`EV-SAFE-*`) live in [`../safety/`](../safety/), a protected path that needs the code owner's review.

The runner loads both folders recursively and validates every file against a strict schema
(`crates/corral-evals`): unknown keys anywhere are errors, so a typo can't quietly drop an expectation.

Cost scenarios can start several runs at once (`runs`, `agents`) at a `budget` (price and caps)
and grade the bill with `expect.spend`: see [docs/05-evals.md](../../docs/05-evals.md) §2, "How cost
scenarios run". `cost-race.yaml` (EV-COST-01) and `cost-fan-out.yaml` (EV-COST-03) are the examples.
