# Testing decision record: unit and live tiers

Status: accepted and implemented.

This record explains why Restream uses two correctness tiers. It is not the
command reference; use [Testing](testing.md) to choose and run a gate.

## Contents

- [Decision](#decision)
- [Why there is no middle tier](#why-there-is-no-middle-tier)
- [Tier responsibilities](#tier-responsibilities)
- [Correctness versus measurement](#correctness-versus-measurement)
- [Implemented outcome](#implemented-outcome)
- [Ongoing rules](#ongoing-rules)

## Decision

Restream has two correctness tiers:

1. **Unit and component tests** run through `cargo test`. They exercise pure
   logic, deterministic state machines, crafted packets/bytes, and bounded
   concurrency models without requiring a running service.
2. **Live tests** start the real `restream` binary, control it through the HTTP
   API, and publish/read media over real localhost RTMP, SRT, HTTP, and file
   boundaries.

Benchmarks are a separate measurement workflow, not a third correctness tier.

## Why there is no middle tier

The former “in-process integration” modes called `MediaEngine::new()` directly
while still using real FFmpeg processes and localhost sockets. They exercised
the same engine code as live tests but bypassed process startup, API wiring,
persistence, and reconciliation. Maintaining both shapes duplicated harness
infrastructure without creating a distinct proof boundary.

An in-memory ingest/egress subsystem was also rejected. Its two useful
properties already have better homes:

- deterministic malformed, reordered, gapped, or truncated input belongs in
  unit/component tests near the parser, demuxer, or ring buffer;
- exact egress assertions belong at a real harness sink receiving the wire
  output of the running binary.

The result is a clearer choice: prove logic without I/O, or prove the assembled
system through its public process and protocol boundaries.

## Tier responsibilities

| Concern | Unit/component tier | Live tier |
|---|---|---|
| Timestamp math and DTS/PTS rules | Primary proof with synthetic packets | Representative wire round-trip |
| Parser/demux fault isolation | Crafted bytes and deterministic errors | Process remains healthy during protocol faults |
| Ring arithmetic and wake/cancel ordering | Unit, property, or loom model | Lifecycle/recovery assertion when externally visible |
| API, database, and reconciliation | Focused handler/service tests where useful | Real binary controlled through `/api/v1/*` |
| Protocol framing and interoperability | Pure codec/container helpers | Real RTMP/SRT/HLS/file traffic and readback |
| Resource shape and throughput | Not a correctness claim | Bench-profile measurement after correctness passes |

The live harness may act as controller, publisher, and sink in one process, but
the system under test remains the separately spawned `restream` binary. A
third-party sink such as MediaMTX or FFmpeg is added only when interoperability
or decode validation is the property being proved.

## Correctness versus measurement

Correctness asks whether a protocol, timestamp, stream selection, lifecycle,
or recovery contract holds. Measurement asks how much CPU, memory, latency, or
throughput a known-correct path consumes.

Keep those workflows separate:

- correctness can use normal test profiles and parallelism when isolation is
  sound;
- measurement uses bench-profile binaries, fixed fixtures, and serial runs;
- a faster result never compensates for a weakened correctness oracle;
- a passing correctness test is not evidence of production-scale capacity.

## Implemented outcome

The migration described by this decision is complete:

- direct `MediaEngine::new()` harness modes were removed or re-tiered;
- pure burst, timestamp, parser, and fault properties live in Rust tests;
- shared child-process, port, fixture, and API helpers drive the real binary;
- `api-smoke` covers authentication, persistence, and lifecycle without media;
- mixed live scenarios combine protocol, codec, graph, HLS, and readback
  assertions instead of spawning a separate pipeline for every property;
- file-ingest and disconnect/recovery behavior have live modes;
- benchmarks remain outside the correctness tier model.

Current mode names, scenario composition, and commands are intentionally not
copied here. The harness catalog and [Testing](testing.md) are the maintained
sources of truth.

## Ongoing rules

- Add a unit/component test when the invariant can be proved without real I/O.
- Add or extend a live scenario when the invariant crosses a process, protocol,
  persistence, or lifecycle boundary.
- Prefer enriching an existing representative live run over adding another
  single-purpose end-to-end pipeline.
- Use checked-in fixtures through `src/test_fixtures.rs`.
- Keep fault injection close to the parser or state machine unless the fault's
  externally visible recovery behavior requires the live tier.
- Treat benchmarks and scale runs as evidence only after the relevant
  correctness gates pass.
