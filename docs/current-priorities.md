# Current priorities

This page records durable priority themes. It is not a second backlog or
architecture map: actionable items and implementation detail stay with their
owning documents.

## Contents

- [Priority themes](#priority-themes)
- [Sources of actionable work](#sources-of-actionable-work)
- [Non-goals](#non-goals)
- [Review rule](#review-rule)

## Priority themes

### Tighten real ownership boundaries

Continue layering work only where it removes observable coupling or duplicated
orchestration. The [layering roadmap](layering-roadmap.md) owns the current
sequence and the [layering audit skill](agent-guidance/skills/layering-audit/SKILL.md)
owns stop rules.

### Preserve shared media work

Share expensive transforms and protocol packaging by typed stage identity while
keeping destination-specific sender state at the edge. Current behavior and
invariants belong in [Media pipeline](media-pipeline.md) and
[Architecture](architecture.md).

### Harden proof and recovery

Prioritize causality-rich diagnostics, deterministic correctness proofs,
fault-isolated recovery, and live protocol evidence. Gate selection belongs in
[Testing](testing.md); open quality work belongs in the
[quality backlog](agent-guidance/quality/backlog.md).

### Keep the Rust and FFmpeg boundary pragmatic

Rust owns orchestration, lifecycle, telemetry, and transport control. FFmpeg
remains appropriate for codec-heavy transforms. Changes to that boundary need
correctness and performance evidence rather than a language-purity goal.

### Treat advanced paths conservatively

Do not advertise custom or incomplete runtime paths as supported without
validation, operator-visible failure behavior, and representative matrix
evidence.

## Sources of actionable work

Use these owners instead of copying their current items here:

- [Quality backlog](agent-guidance/quality/backlog.md) for prioritized,
  executable hardening items;
- [Layering roadmap](layering-roadmap.md) for ordered ownership refactors;
- [Stage boundary proof map](stage-boundary-proof-map.md) for proof gaps;
- [Regression artifacts](regression-artifacts.md) for historical replay
  obligations.

This page changes only when the project's priority themes change.

## Non-goals

The following themes do not justify work by themselves:

- “finish the rewrite” as a broad program;
- preserve removed Node.js or MediaMTX runtime mental models;
- split modules or crates without clearer ownership;
- replace FFmpeg for ideological reasons;
- duplicate active backlog items in another planning document;
- revive completed migration plans as current guidance.

## Review rule

Review this page at major releases or architectural pivots. Routine item
completion belongs in its owning backlog, roadmap, proof map, or evidence
record—not in a status diary here.
