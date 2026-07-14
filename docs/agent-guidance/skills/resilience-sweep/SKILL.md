---
name: resilience-sweep
description: Prove or fix ONE fault-isolation/recovery behavior — crafted-bytes fault injection, disconnect/reconnect, teardown status, panic containment, or a live-harness fault mode. Use for backlog items tagged [resilience], or when asked to harden failure paths, chaos-test the engine, or verify recovery behavior.
---

# Skill: resilience-sweep

The engine's contract: **no internal or external failure path may crash the
engine; faults are isolated and surfaced as errors** (AGENTS.md). At broadcast
scale every rare fault becomes a certainty — a one-in-a-million packet arrives
hundreds of times per event. One invocation proves or fixes one failure path.

## The resilience proof layers

1. **Crafted-bytes unit tests** — feed malformed, truncated, reordered, or
   gapped bytes to demuxers, parsers, and ring buffers directly. This is the
   designated home for fault injection (`docs/testing-strategy.md`); ffmpeg
   won't emit malformed data on demand.
2. **Lifecycle unit tests** — teardown, cancellation, double-close, late
   reader, re-publish while draining; assert operator-visible status.
3. **Live harness fault modes** (build harness first, run under
   `scripts/build/resource-limit.sh`, private netns by default):
   - `fault.resilience` — general fault isolation
   - `fault.egress-retry` — egress destination failure/retry
   - `fault.output-stall` — stalled output handling
   - `recovery` — disconnect/recovery semantics
   - `signal.control` — signal-driven lifecycle
4. **Panic containment audit** — every FFmpeg/libsrt OS-thread entry point
   must be wrapped in `catch_unwind(AssertUnwindSafe(...))`.

## Execution recipe (backlog item in hand)

1. State the failure scenario precisely: what arrives/breaks, at which
   boundary, and what the engine must do (isolate + surface, never crash).
2. Reproduce first: write the failing test before touching engine code. For
   crafted-bytes cases, build the malformed input from a valid fixture
   (via `src/test_fixtures.rs`) and corrupt the specific field — random fuzz
   blobs make poor regression tests.
3. Fix the narrowest code path. Errors must surface through the existing
   status/telemetry contract, not new ad-hoc channels.
4. If teardown or recovery semantics changed: update the live harness
   assertion and the operator-visible status contract in the same change
   (`tests/api.rs`, `docs/api-reference.md`, `docs/observability.md`).
5. Gates: the new test, scoped `cargo test`, plus
   `bash ./scripts/check/concurrency/fast.sh` if lifecycle/cancellation
   was touched, plus the relevant harness fault mode if the change is about
   live behavior.

## Discovery recipe (when asked to find new [resilience] items)

Run ONE probe, file items, fix nothing:

- Enumerate demux/parse entry points in `src/media/` and check each has at
  least one malformed-input test (truncated header, oversized declared length,
  invalid tag type, non-monotonic timestamps, mid-stream parameter change).
- Grep FFmpeg/libsrt OS-thread spawns and verify `catch_unwind` coverage.
- Run one harness fault mode and compare its assertions against
  `docs/stage-boundary-proof-map.md` and `docs/regression-artifacts.md`;
  behaviors documented but unasserted become items.
- Check reconnect paths: source re-publish, egress retry backoff, SRT bond vs
  duplicate-publisher handling (duplicates are NOT bonds — AGENTS.md).

## Rules

- Never assert only "does not crash" — also assert the fault is *surfaced*
  (error status, telemetry counter, log at the right level per log-audit).
- Keep fault tests deterministic: no sleeps-as-synchronization, no real-time
  races; drive lifecycle explicitly.
- Media timestamps stay separate from wall-clock time in any recovery logic.
- Do not add recovery behavior speculatively; prove the fault path exists
  first, then handle it.
