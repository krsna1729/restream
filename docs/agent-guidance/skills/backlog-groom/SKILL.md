---
name: backlog-groom
description: Mine the repo's docs, gates, coverage, and journal for new quality-backlog items; re-prioritize, merge duplicates, unblock or escalate stale items. Use for backlog items tagged [groom], when the quality backlog is empty for a dimension, or when asked to groom/refill/reprioritize the quality backlog.
---

# Skill: backlog-groom

The backlog is the loop's fuel. Grooming keeps it honest: every item small
enough for one iteration, verifiable, correctly tiered, and worth doing.
One invocation = one grooming pass (≤5 new items, plus hygiene).

## Item quality bar (reject drafts that miss any of these)

A well-formed item in `docs/agent-guidance/quality/backlog.md`:

```
### Q-NNN [dimension] [tier] <short imperative title>
- Goal: <observable end state, one sentence>
- Files: <the files/modules involved, so no exploration is needed to start>
- Gates: <the exact commands that must pass to call it done>
- Context: <why it matters + pointers to docs/commits; enough to work cold>
- Status: open
```

- Dimension ∈ `proof | resilience | modularity | efficiency | performance | groom`
- Tier ∈ `haiku` (read-only audit/inventory/docs) · `sonnet` (scoped code+test)
  · `opus` (concurrency/lifecycle redesign, hot-path architecture,
  benchmark-driven decisions — per AGENTS.md model guidance)
- Sized for ONE iteration. If it needs "and then", split it.
- No item may instruct weakening a gate, skipping verification, or touching
  another agent's in-flight work.

## Source mines (pick the dimension that needs items, run its mines)

- **proof:** discovery recipes in the proof-sweep skill (coverage map,
  panic-path inventory, invariant cross-check, gate-coverage diff against
  `docs/concurrency-proof-coverage-2026-07-02.md`).
- **resilience:** discovery recipes in resilience-sweep; plus
  `docs/run-to-completion-analysis.md` and `docs/resource-sweep.md` for
  documented-but-unasserted behaviors.
- **modularity:** discovery recipes in modularity-sweep;
  `docs/layering-roadmap.md` topmost undone steps.
- **efficiency/performance:** stale ledger rows and standing opportunities in
  `docs/agent-guidance/quality/baselines.md`.
- **all:** `journal.md` FAILED/blocked entries older than 3 days — either
  write a sharper re-scoped item, escalate the tier tag, or record why it
  should stay parked.
- **all:** `git log --oneline -30` — recent changes in hot paths or lifecycle
  code with no accompanying proof/bench are candidate items.

## Hygiene pass (every groom)

1. Move `done` items older than ~2 weeks into the "Archive" section at the
   bottom of `backlog.md` (never delete — commit hashes live there).
2. Merge duplicates; keep the better-specified one, note the merge.
3. Confirm tier tags: anything touching Tokio↔OS-thread handoff, wake/cancel,
   stage registries, or packet-loop architecture is `[opus]`, no exceptions.
4. Re-order: highest leverage first. Proof gaps on invariants that guard live
   broadcasts outrank cosmetic wins. Keep all dimensions represented in the
   top 10 if material exists.
5. Cap: if >10 open items exist for one dimension, stop mining that dimension.

## Rules

- Grooming files work; it never does the work. No code edits in a groom pass.
- Every filed item must be executable cold by a model that has read nothing
  but the item and its skill — test each draft against that bar.
- Date-stamp filed items (`Filed: YYYY-MM-DD by groom`).
- Journal the pass (`GROOMED`, list of ids filed/merged/archived) per the
  quality-loop format.
