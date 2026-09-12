---
name: proof-sweep
description: Find or close correctness and fault-recovery proof gaps in restream, including quality-backlog proof/resilience items and targeted panic-containment audits.
---

# Proof Sweep

Start with the production invariant or failure scenario and its observable
outcome. For audit/discovery requests, report or file evidence-backed gaps;
implement only when fixes or a backlog iteration are requested.

Choose the lightest proof that reaches the real failure:

- Unit tests for pure packet/parser/timestamp logic.
- Proptest for input-dependent invariants.
- Loom for synchronization interleavings.
- Live harness scenarios for real sockets, processes, teardown, and recovery.

A regression should fail before the fix. For a previously untested invariant,
check a representative counterexample or controlled mutation when needed to
establish that the assertion can detect the fault. Do not require production
mutation for every test; restore any temporary mutation without disturbing
other work.

For faults, assert both containment and the existing error/status signal.
Use deterministic synchronization. Build malformed inputs deliberately (or
corrupt a checked-in fixture); keep valid media fixture resolution in
`src/test_fixtures.rs`. Check FFmpeg/libsrt OS-thread entry points for panic
containment where those backends are used.

Select verification from [AGENTS.md](../../../../AGENTS.md). For concurrency or
lifecycle changes, use [concurrency-proof](../concurrency-proof/SKILL.md);
for live protocol/fault scenarios, use [protocol-test](../protocol-test/SKILL.md).
Teardown/recovery changes need live assertions and operator-status contracts
in the same change.

For discovery, compare [the proof map](../../../stage-boundary-proof-map.md)
and [regression artifacts](../../../regression-artifacts.md) against actual
tests/gates. Inspect fallible production panic paths, excluding test helpers.
Use coverage only to locate unproven behavior; do not chase a percentage.

See [testing strategy](../../../testing-strategy.md) for proof-layer ownership
and [testing](../../../testing.md) for fixtures and commands.
