---
name: concurrency-proof
description: Prove changes to synchronization, task/thread handoffs, cancellation, stage registries, or teardown/recovery status in restream.
---

# Concurrency Proof

Identify the ordering or ownership invariant and the observable failure.
Use production code through existing seams; a separate test implementation
does not prove the production path.

- Add deterministic lifecycle/status tests.
- Use loom for relevant wake/cancel or registry interleavings; bound the model.
- For recovery or teardown changes, update the live harness fault assertion
  and operator-visible status contract together.
- Extend the proof gate for a new primitive or thread hop, or explain how
  existing coverage enforces it.
- Benchmark hot-path changes; otherwise state that the change is off the hot path.

Follow [AGENTS.md](../../../../AGENTS.md) for host/build safety. Run the focused
proof gate, then the live contract gate before signing off on lifecycle or
recovery changes:

```sh
scripts/check/concurrency/fast.sh
scripts/check/concurrency/contract.sh
```

Read [concurrency proofing](../../../concurrency-proofing.md) for model targets,
gate coverage, and status-contract details. Select API/frontend checks from the
changed contract; do not infer recovery success solely from process survival.
