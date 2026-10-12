---
name: concurrency-proof
description: Prove changes to synchronization, task/thread handoffs, cancellation, stage registries, or teardown/recovery status in restream.
---

# Concurrency Proof

Identify the ordering or ownership invariant and the observable failure.
Use production code through existing seams; a separate test implementation
does not prove the production path.

- Add deterministic lifecycle/status tests.
- Use loom or proptest for relevant wake/cancel or registry interleavings;
  bound the model.
- For recovery or teardown changes, update the live harness fault assertion
  and operator-visible status contract together.
- Extend the proof gate for a new primitive or thread hop (the step tables
  in `crates/xtask/src/concurrency.rs`), or explain how existing coverage
  enforces it.
- Benchmark hot-path changes; otherwise state that the change is off the hot path.

Follow [AGENTS.md](../../../../AGENTS.md) for host/build safety. Locally, run
only the new step or loom case you added (`cargo test <new test name>`). CI
runs the fast gate on every PR and the live contract gate on master pushes:

```sh
cargo xtask concurrency fast
cargo xtask concurrency contract
```

Read [concurrency proofing](../../../concurrency-proofing.md) for model targets,
gate coverage, and status-contract details. Select API/frontend checks from the
changed contract; do not infer recovery success solely from process survival.
