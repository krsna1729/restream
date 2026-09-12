---
name: backlog-groom
description: Groom the quality backlog when asked to find, prioritize, merge, or rescope maintenance work, or when a quality-loop iteration has no eligible item.
---

# Backlog Groom

Update [the backlog](../../quality/backlog.md) from current evidence. This is a
planning pass; do not implement the items it discovers.

A new item needs a concrete failure, missing proof, measured regression, or
ownership problem. Age, file size, and coverage percentage alone are not reasons
to create work. Inspect relevant source and gates before filing it.

Use the existing format:

```text
### Q-NNN [dimension] [tier] <action>
- Goal: <observable result>
- Files: <starting files or modules>
- Gates: <runnable commands and acceptance criteria>
- Context: <evidence and relevant references>
- Status: open (Filed: YYYY-MM-DD)
```

Dimensions are `proof`, `resilience`, `modularity`, `efficiency`,
`performance`, and `groom`. Keep the legacy tier tags; interpret them by
task capability in [AGENTS.md](../../../../AGENTS.md), not a fixed model version.

For discovery, inspect the relevant source of evidence:

- Proof/recovery: [stage boundary proof map](../../../stage-boundary-proof-map.md)
  and [regression artifacts](../../../regression-artifacts.md); check whether
  production invariants and observable recovery have executable assertions.
- Ownership: [layering roadmap](../../../layering-roadmap.md); verify the
  coupling still exists.
- Performance: [baselines](../../quality/baselines.md); distinguish old
  measurements from a demonstrated regression.
- Blocked work: [journal](../../quality/journal.md); preserve useful negative
  results and rescope items whose blockers have changed.

Keep items small enough to validate independently. Merge duplicates, preserve
completion/commit evidence in the archive, and prioritize broadcast correctness
over cosmetic cleanup. Respect active claims. Journal a grooming pass without
creating work merely to fill every dimension or meet a quota.
