# Testing evidence snapshots — 2026-06-20 to 2026-07-01

> **Status: historical evidence.** These route, coverage, validation, resource,
> and rollout snapshots were moved out of the maintained testing guide because
> their counts and source maps naturally drift. Verify current behavior with
> the commands and source-of-truth pointers in [testing.md](../testing.md).

## Contents

- [API Route Coverage Matrix](#api-route-coverage-matrix)
- [Code Coverage](#code-coverage)
- [Validation Results: June 20, 2026](#validation-results-june-20-2026)
- [End-to-End Test Plan](#end-to-end-test-plan)
- [Current Resource Measurements (2026-06-28)](#current-resource-measurements-2026-06-28)
- [Media Correctness Findings (2026-07-01)](#media-correctness-findings-2026-07-01)

## API Route Coverage Matrix

This matrix is a point-in-time test-coverage view, not the canonical route
inventory. Routes are registered in `src/api/router.rs`, with unit coverage in
`tests/api.rs` and live coverage in `src/bin/test_harness/`. The router
currently declares 10 public and 70 authenticated paths; this table has not yet
been expanded to cover every path. Legend: ✓ = covered, — = not covered,
~ = precondition only.

**Auth**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `POST` | `/api/auth/login` | ✓ | ✓ | |
| `POST` | `/api/auth/logout` | ✓ | — | |
| `POST` | `/api/auth/change-password` | ✓ | — | |

**Config**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/settings` | ✓ | ✓ | |
| `PATCH` | `/api/v1/settings` | ✓ | — | 3 tests incl. transcode profiles |
| `GET` | `/audio-caps` | ✓ | — | |
| `GET` | `/api/v1/stream-keys` | ✓ | — | |

**Pipelines**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/pipelines` | ✓ | ✓ | |
| `POST` | `/api/v1/pipelines` | ✓ | ✓ | Create |
| `PATCH` | `/api/v1/pipelines/:id` | ✓ | — | Update |
| `DELETE` | `/api/v1/pipelines/:id` | ✓ | ✓ | fault.resilience SRT test |

**File ingest**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/pipelines/:id/file-ingest` | ✓ | — | |
| `PUT` | `/api/v1/pipelines/:id/file-ingest` | ✓ | ✓ | |
| `DELETE` | `/api/v1/pipelines/:id/file-ingest` | ✓ | — | |

**Outputs**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `POST` | `/api/v1/pipelines/:id/outputs` | ✓ | ✓ | Create |
| `PATCH` | `/api/v1/pipelines/:id/outputs/:oid` | ✓ | — | Update |
| `DELETE` | `/api/v1/pipelines/:id/outputs/:oid` | ✓ | — | |
| `POST` | `/api/v1/pipelines/:id/outputs/:oid/start` | ✓ | ✓ | |
| `POST` | `/api/v1/pipelines/:id/outputs/:oid/stop` | ✓ | ✓ | |
| `GET` | `/api/v1/pipelines/:id/outputs/:oid/status` | ✓ | ✓ | |

**Pipeline detail**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/pipelines/:id/probe` | — | ✓ | mixed-input, correctness-* |
| `GET` | `/api/v1/pipelines/:id/graph` | ✓ | ✓ | |
| `GET` | `/api/v1/pipelines/:id/alerts` | ✓ | — | auth + response shape |
| `POST` | `/api/v1/pipelines/:id/diagnostics/run` | ✓ | — | Auth, method, JSON response shape, and busy `429` |
| `POST` | `/api/v1/pipelines/:id/recording/start` | — | ✓ | mixed.live.srt.h264.a1.bf2 |
| `POST` | `/api/v1/pipelines/:id/recording/stop` | — | ✓ | mixed.live.srt.h264.a1.bf2 |

**Encodings**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/encodings/custom` | ✓ | — | |
| `PUT` | `/api/v1/encodings/custom` | ✓ | — | |

**Ingests**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/ingests` | ✓ | ✓ | |
| `POST` | `/api/v1/ingests` | ✓ | — | |
| `PUT` | `/api/v1/ingests/:id` | ✓ | — | |
| `DELETE` | `/api/v1/ingests/:id` | ✓ | — | |
| `POST` | `/api/v1/ingests/:id/start` | ✓ | ✓ | |
| `POST` | `/api/v1/ingests/:id/stop` | — | ✓ | fault.resilience |

**Status and health**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/engine` | ✓ | — | |
| `GET` | `/api/v1/engine/sbom` | ✓ | — | |
| `GET` | `/api/v1/media` | ✓ | — | |
| `POST` | `/api/v1/media/upload` | ✓ | ✓ | authenticated multipart upload; duplicate/path traversal proof |
| `GET` | `/api/v1/media/:filename/analysis` | ✓ | — | |
| `PATCH` | `/api/v1/media/:filename` | ✓ | — | Rename + ingest reference update |
| `DELETE` | `/api/v1/media/:filename` | ✓ | — | Path traversal tested |
| `GET` | `/api/v1/dashboard/runtime` | ✓ | — | Frontend contract + Node transport tests |
| `GET` | `/api/v1/engine/health` | ✓ | ✓ | |
| `GET` | `/healthz` | ✓ | ✓ | |
| `GET` | `/metrics/system` | ✓ | — | Structured cpu/memory/disk/network |

**V1 operator API**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/logs` | — | — | New; unit tests pending |
| `GET` | `/api/logs/stream` | — | — | SSE; new; unit tests pending |
| `GET` | `/api/v1/alerts` | ✓ | — | Aggregate across all pipelines |
| `GET` | `/api/v1/events` | ✓ | — | Filtering tested |
| `GET` | `/api/v1/overview` | ✓ | — | |
| `GET` | `/api/v1/engine/telemetry` | ✓ | — | |
| `GET` | `/api/v1/pipelines/:id/telemetry` | ✓ | — | |
| `GET` | `/api/v1/stages/:key/telemetry` | ✓ | — | |
| `GET` | `/api/v1/pipelines/:id/summary` | ✓ | — | |

**Agent API**

| Method | Route | Unit | Live | Notes |
|---|---|:---:|:---:|---|
| `GET` | `/api/v1/agent/capabilities` | ✓ | — | |
| `GET` | `/api/v1/agent/context` | ✓ | — | |
| `POST` | `/api/v1/agent/investigations` | ✓ | — | |
| `POST` | `/api/v1/agent/plans` | ✓ | — | |
| `POST` | `/api/v1/agent/plans/validate` | ✓ | — | |
| `POST` | `/api/v1/agent/graph-diff-preview` | ✓ | — | 404 when compiled out |
| `POST` | `/api/v1/agent/operations` | ✓ | — | |
| `GET` | `/api/v1/agent/operations/:id` | ✓ | — | |
| `POST` | `/.../operations/:id/approve` | ✓ | — | |
| `POST` | `/.../operations/:id/apply` | ✓ | — | |
| `POST` | `/.../operations/:id/verify` | ✓ | — | |
| `POST` | `/api/v1/agent/verify` | ✓ | — | 404 when compiled out |

Frontend transport/control layering now has explicit Node-scope coverage for:
- the combined `/api/v1/dashboard/runtime` fetch shape that replaces paired dashboard health+metrics reads
- selected-pipeline runtime refreshes using `pipeline_id` to keep sibling pipeline summaries live while enriching the active pipeline entry with full detail
- output start/stop mutations reusing lifecycle SSE convergence with a runtime-refresh fallback instead of always forcing an immediate runtime GET
- recording start/stop mutations patching local operator state directly instead of forcing a runtime refresh
- file-ingest start/stop falling back to runtime refreshes only when no lifecycle stream is already open
- output toggle responsiveness while start/stop API requests are in flight
- output create/update mutations reusing returned payloads instead of refetching dashboard settings
- pipeline create/update mutations reusing returned payloads instead of refetching dashboard settings
- pipeline and output deletes patching dashboard state locally instead of refetching dashboard settings
- restream process-indicator transitions driven by lifecycle logs and health recovery
- restream process-indicator reachability updates from metrics-only non-runtime modes
- non-runtime mode lifecycle SSE behavior that keeps process state live without re-enabling health polls
- status mode avoiding a duplicate lifecycle-only SSE by reusing its restream log stream
## Code Coverage

Line coverage from `cargo llvm-cov` (unit tests only, June 29, 2026):

Compared with the June 27, 2026 snapshot, covered lines increased from
`13,250` to `13,784` (`+534`), but total instrumented lines increased from
`23,918` to `25,399` (`+1,481`), so overall unit-only line coverage moved from
`55.4%` to `54.3%` (`-1.1` percentage points).

![Coverage by module](../coverage-by-module.svg)

| Module | Lines | Covered | Coverage |
|---|---:|---:|---:|
| `pipe_metrics` | 21 | 21 | 100.0% |
| `engine_registries` | 49 | 49 | 100.0% |
| `events` | 284 | 274 | **96.5%** |
| `alerts` | 517 | 506 | 97.9% |
| `security` | 220 | 210 | 95.5% |
| `ring_buffer` | 1,096 | 1,040 | 94.9% |
| `feeder` | 226 | 215 | 95.1% |
| `file_ingest` | 558 | 515 | 92.3% |
| `codec` | 730 | 660 | 90.4% |
| `mpegts` | 2,444 | 2,028 | 83.0% |
| `hls_upload` | 232 | 207 | 89.2% |
| `profiles` | 333 | 285 | 85.6% |
| `stage_metrics` | 44 | 37 | 84.1% |
| `engine` | 3,551 | 2,743 | 77.3% |
| `domain/stage` | 227 | 180 | 79.3% |
| `avio` | 502 | 388 | 77.3% |
| `hls` | 565 | 428 | **75.8%** |
| `recording` | 309 | 193 | **62.5%** |
| `external_transcoder` | 581 | 364 | **62.7%** |
| `srt` | 2,471 | 1,183 | 47.9% |
| `rtmp` | 1,660 | 644 | 38.8% |
| `api` | 3,951 | 385 | 9.7%† |
| `db` | 801 | 0 | 0.0%† |
| **Total** | **25,399** | **13,784** | **54.3%** |

† `api.rs` is tested via 66 integration tests in `tests/api.rs` which `llvm-cov --lib` does not instrument. `db.rs` is tested via `tests/db.rs`. Their unit-only coverage is not representative.

These numbers reflect unit-test-only instrumentation. `api.rs` shows 7% because
`cargo llvm-cov` does not instrument `tests/api.rs` integration tests by
default — the real API test coverage is much higher (66 tests across all 59
routes). Similarly, `db.rs`, `rtmp.rs`, and `srt.rs` are primarily exercised by
the live integration harness which is not captured by `llvm-cov`.

### Coverage interpretation

- **≥80% (14 modules)**: core media pipeline logic — ring buffer, codec,
  MPEG-TS, engine, HLS upload, file ingest, alerts, events, security, profiles,
  feeder, stage_metrics. Well covered by unit tests.
- **50–79% (6 modules)**: socket-heavy protocol handlers, HLS store, and
  recording logic. Primarily exercised by the live harness with real ffmpeg;
  unit-testing their socket loops would require significant mocking for little
  added benefit.
- **<50% (7 modules)**: API/DB/diagnostics layers tested through integration
  tests not captured by `llvm-cov`, or FFmpeg-dependent transcoder code that
  requires the binary running.
## Validation Results: June 20, 2026

Environment: WSL2, 20 logical CPUs, 7.6 GiB RAM, 2 GiB swap.

### Correctness

An eight-second generated H.264/AAC MPEG-TS file was looped through real FFmpeg
publishers.

| Test | Result | External `ffprobe` |
|---|---|---|
| File → RTMP ingest → RTMP read | PASS | H.264 640x360 + AAC 48 kHz mono |
| File → SRT ingest → SRT read | PASS | H.264 640x360 + AAC 48 kHz mono |
| RTMP source → RTMP egress → RTMP sink read | PASS | H.264 640x360 + AAC 48 kHz mono |
| RTMP source → SRT egress → SRT sink read | PASS | H.264 640x360 + AAC 48 kHz mono |

Every probe contained exactly one video and one audio stream.

### In-Process Load

```text
500 RingBuffer readers, 2,000 source packets, 1,316-byte payload
→ 1,000,000/1,000,000 deliveries, 1.316 GB logical, 51.36 M deliveries/s
→ 27,516 KiB peak RSS
```

### Bounded Network Load

```text
32 RTMP egress sessions, in-process RTMP handshake-and-discard sink, 5s hold
→ 32/32 connections, 9,408 media messages, 9.686 Mbps aggregate
→ 28,800 KiB peak RSS
```

### FFmpeg Assembly Benchmark (June 21, 2026)

Matched static FFmpeg 6.1.5, pinned single-CPU, median of seven runs:

| Workload | No x86 asm | x86 asm | Speedup |
|---|---:|---:|---:|
| 4K HEVC decode, 3s | 2.48 s | 1.27 s | 1.95× |
| 1080p H.264 decode, 5s | 0.62 s | 0.29 s | 2.14× |
| 4K HEVC decode + 1080p scale, 2s | 3.82 s | 1.22 s | 3.13× |
| 4K HEVC → 1080p H.264/x264, 2s | 5.45 s | 2.49 s | 2.19× |
## End-to-End Test Plan

### Deterministic Fixtures

**Dual-Audio H.264:**
```bash
ffmpeg -y \
  -f lavfi -i "testsrc2=size=1920x1080:rate=30" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000" \
  -f lavfi -i "sine=frequency=880:sample_rate=48000" \
  -t 120 \
  -map 0:v -map 1:a -map 2:a \
  -c:v libx264 -preset slow -g 60 -bf 2 \
  -c:a aac -b:a 128k \
  -metadata:s:a:0 title=track-440hz \
  -metadata:s:a:1 title=track-880hz \
  .local/artifacts/dual-audio-h264.mkv
```

**Dual-Audio H.265:**
```bash
ffmpeg -y \
  -f lavfi -i "testsrc2=size=1920x1080:rate=30" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000" \
  -f lavfi -i "sine=frequency=880:sample_rate=48000" \
  -t 120 \
  -map 0:v -map 1:a -map 2:a \
  -c:v libx265 -preset slow -x265-params "keyint=60:bframes=2" \
  -c:a aac -b:a 128k \
  .local/artifacts/dual-audio-h265.mkv
```

Also retain short 10-second versions for smoke tests.

### Phase 1: Ingest Equivalence

Publish the same H.264 fixture to both RTMP and SRT pipelines. Verify:

- both active within 10 seconds
- correct protocol reported
- bytes and bitrate increase continuously
- process survives sequence headers, B-frames, reconnects, and shutdown
- no subtitle, data, or unknown streams in the media ring

### Phase 2: Probe Matching

Use both engine snapshots (`/api/v1/pipelines/:id/probe`) and external `ffprobe` via
matching protocol. Compare: video codec, dimensions, frame rate, audio codec,
sample rate, channels, track count, GOP interval.

| Field | Tolerance |
|---|---|
| Codec, dimensions, sample rate, channels | Exact |
| Frame rate | ±0.01 fps |
| GOP interval | ±1 frame |
| Average bitrate | ±10% after warm-up |
| A/V start offset | ≤ 50 ms |
| A/V drift over 10 min | ≤ 20 ms |

### Phase 3: Egress Correctness Matrix

2 ingests × 6 video shapes × 6 audio modes × 3 protocols = 216 cases.
Use pairwise reduction for CI; full Cartesian nightly. Always include collision
cases (`720p+atrack:0`, `720p+atrack:1`, `1080p+atrack:0`, `source+atrack:0`)
to prove stage sharing and audio isolation.

Per-output assertions:

- correct stream count and types
- resolution matches preset
- all packets decode for 30s with `-xerror`
- DTS monotonic per stream
- valid PTS/DTS reordering for B-frames
- A/V start offset ≤ 50 ms
- no drift beyond 20 ms over long test
- stopping one output does not interrupt shared stages

Audio routing content assertions (via `astats`, `channelsplit`, frequency
detection):

| Routing | Assertion |
|---|---|
| `passthrough` | Both 440 Hz and 880 Hz tracks remain |
| `atrack:0` | Only 440 Hz |
| `atrack:1` | Only 880 Hz |
| `atrack:0,1` | Both in requested order |
| `remap:0:1:0` | Correct channel derivation |
| `downmix:0` | Stereo with expected contribution |

### Phase 4: H.265 Coverage

Publish H.265 via SRT. Verify SRT passthrough preserves HEVC identity, RTMP
egress capability test, no silent HEVC-as-H.264 mislabeling.

`cargo run --bin test_harness -- mixed.live.srt.h265.a1.bf2` covers both HEVC
edges from a single scenario: it ingests H.265 over SRT and, through its
multi-protocol output plan, verifies the RTMP leg (shared `hevc_to_h264` stage,
H.264 video plus AAC audio at the RTMP read endpoint) and the SRT leg (native
HEVC passthrough, HEVC video plus AAC audio at the SRT read endpoint) together.

`cargo run --bin test_harness -- mixed.live.srt.h264.a1.bf0` covers the direct
cross-protocol packetization path: it ingests H.264/AAC over SRT, loops it
through RTMP egress, and verifies H.264 video plus AAC audio at the RTMP read
endpoint.

### Phase 5: Recovery and Isolation

- publisher stop/restart
- sink restart during active outputs
- 1%, 3%, 5% packet loss + 50 ms jitter on SRT
- add/remove outputs sharing video stages
- one slow sink does not stall others
- readers recover at keyframe after ring overflow
- shared stages survive while dependents exist, terminate after last stops

### Phase 6: Scale Benchmarks

**In-process** (no network): 500 null consumers, deterministic packet replay.
Measures engine CPU/memory independent of network.

**Networked**: custom separate-process sink (RTMP/SRT/HLS PUT listeners),
ramp 1→10→50→100→250→500 outputs, hold 30 min at 500, 2-hour soak.

Functional gates: 500/500 publishing, all receive bytes, no unexpected
termination, aggregate bitrate ±5%, no ring overflow, resources return to
baseline on stop.

### Automation

Currently checked in:

```text
scripts/build/resource-limit.sh target/debug/test_harness ramp-family
scripts/build/resource-limit.sh target/bench/test_harness mixed.live.srt.h264.a1.bf2
scripts/build/resource-limit.sh target/debug/test_harness bonding
scripts/build/resource-limit.sh target/debug/test_harness timestamp.bframe
scripts/build/resource-limit.sh target/debug/test_harness mixed.live.srt.h264.a1.bf0
scripts/build/resource-limit.sh target/debug/test_harness mixed.live.srt.h265.a1.bf2
./target/bench/test_harness resource-sweep
./target/bench/test_harness bitrate-sweep
scripts/harness/media-validation.sh
```

Aggregate release-evidence runner:

```sh
scripts/harness/run.sh suite -- --run-id <run-id>
```

Use `test_harness suite` as the canonical aggregate orchestrator. It creates
`.local/artifacts/<run-id>/manifest.json`, runs each checked-in integration mode
in its own subdirectory, and records one JSONL result per mode in
`.local/artifacts/<run-id>/results.jsonl`. Supported suite options are:

- `--run-id <id>` to choose the artifact run id
- `--work-root <path>` to choose the aggregate artifact directory
- `--only-modes mixed.live.srt.h264.a1.bf2,timestamp.bframe` to run a subset
- `--preflight-only` to run readiness checks without starting live services
- `--mode-timeout-secs <seconds>` to override the bounded 15-minute child-mode
  timeout (`TEST_HARNESS_SUITE_MODE_TIMEOUT_SECS` provides the same override)
- `--continue-on-fail` to keep collecting artifacts after the first failure

Default release-evidence modes run in ascending expected-duration order so CI
surfaces cheap failures before long measurement sweeps. The order is declared
as `suiteOrder` in `test/harness/modes.json`; do not rely on JSON object order,
because the catalog lookup is alphabetized internally.

| Order | Mode | Last local release duration |
|---:|---|---:|
| 1 | `api-smoke` | 2s |
| 2 | `file.live-edge` | 32s |
| 3 | `srt.policy` | 55s |
| 4 | `branch-matrix` | 1m 29s |
| 5 | `fault.resilience` | 3m 12s |
| 6 | `srt-crypto-matrix` | 5m 54s |
| 7 | `ramp-family` | 7m 5s |
| 8 | `resource-sweep` | 7m 43s |
| 9 | `bitrate-sweep` | 14m 1s |
| 10 | `mixed.matrix` | 49m 6s |

Parallel-safe correctness modes may still overlap when namespace isolation is
available, but the suite writes their JSONL results in completion order so the
fastest checks become visible as soon as they finish.

Heavyweight suite-default modes can declare a larger catalog timeout floor in
`test/harness/modes.json`. `mixed.matrix` does this because it performs the
full RTMP/SRT, H.264/H.265, audio-track, HLS, recording, and decode-scan
matrix. `resource-sweep` also declares a floor because the release-default
growth cases include source, transcode, and dual-transcode stacks.
`bitrate-sweep` declares the same floor because it runs real publisher, output,
sampling, and probe loops across multiple bitrate points and can finish near
the default 15-minute ceiling on a fast local machine. These floors are caps,
not sleeps: they only prevent the suite from killing a mode that is still
making expected progress. The default 15-minute cap is still used for ordinary
modes.

The aggregate manifest and each JSONL row label their evidence as `preflight`
or `execution`; a successful `--preflight-only` run therefore cannot be
mistaken for proof that the live mode ran. Timed-out children are terminated as
one owned process group and write `timeout.json` beside `run.log` before the
suite continues or fails.

Why the aggregate runner lives in `test_harness` instead of a separate
`protocol_matrix` binary:

- The suite and the per-mode scenarios already share the same artifact layout,
  loopback namespace handling, fixture generation, child-process helpers, and
  result serialization.
- Keeping orchestration and mode execution in one binary avoids a second Rust
  surface that can drift in CLI semantics, manifest shape, or per-mode naming.
- `suite_run()` can spawn the same executable for each mode, which keeps the
  aggregate runner honest: it exercises the exact per-mode entrypoints used in
  focused runs instead of re-implementing them in a parallel binary.
- The old shell-plus-`protocol_matrix` path no longer buys us anything. The
  aggregate orchestration logic is already implemented in `test_harness`, so
  the extra wrapper only adds another compatibility surface to maintain.

`mixed.live.srt.h264.a1.bf2`, `timestamp.bframe`, `mixed.live.srt.h264.a1.bf0`,
and `mixed.live.srt.h265.a1.bf2` are behind typed Rust
harness entry points, and `ramp-family` runs the full eight-config ramp matrix.
`mixed.live.srt.h264.a1.bf2` owns the former anchor probe bundle.

`test_harness` writes `manifest.json` in the selected `WORK_DIR`
for each checked-in mode. The manifest starts as `RUNNING` and is finalized to
`PASS` or `FAIL` with timestamps, git head, network mode, and primary artifact
paths. This applies even to setup failures after the mode has initialized its
artifact directory, making failed matrix attempts auditable instead of silent.

Planned scenario families for the remaining matrix should be added as
`test_harness` entries:

```text
ingest-equivalence
egress-matrix
h265
recovery
scale-inprocess
scale-500
```

Each completed matrix run should write artifacts to `.local/artifacts/<run-id>/` with manifest,
environment, per-case results (PASS/FAIL/EXPECTED_FAIL/SKIPPED/INFRA_FAILURE),
ffprobe output, captures, metrics, logs, and summary.
## Current Resource Measurements (2026-06-28)

These numbers are authoritative current-code measurements generated by the Rust
harness:

- `.local/artifacts/resource-sweep-authoritative/resource-sweep-results.csv`
- `.local/artifacts/bitrate-sweep-authoritative/bitrate-sweep-results.csv`

Both sweeps use live ingest/egress, sample `/proc`, and cross-check against
`/api/v1/engine/telemetry`.

### Resource Sweep Snapshot

Isolated sweep, current default code:

| Scenario | Restream MB | Child FFmpeg MB | Combined MB | Restream CPU % | Child FFmpeg CPU % | Total CPU % |
|---|---:|---:|---:|---:|---:|---:|
| Empty baseline | 72.8 | 0.0 | 72.8 | 1.15 | 0.00 | 1.15 |
| Same ingest growth, 5x H.264 SRT | 82.6 | 0.0 | 82.6 | 7.27 | 0.00 | 7.27 |
| Mixed ingest growth, 5 ingest types | 75.9 | 0.0 | 75.9 | 8.92 | 0.00 | 8.92 |
| Mixed source egress, 20 outputs | 83.8 | 0.0 | 83.8 | 11.86 | 0.00 | 11.86 |
| Mixed 720p transcode egress, 20 outputs | 120.3 | 166.5 | 286.8 | 17.96 | 33.69 | 51.65 |
| HEVC bridge, 10 RTMP source outputs | 158.7 | 0.0 | 158.7 | 71.82 | 0.00 | 71.82 |

Current queue/ring peaks for those same rows:

| Scenario | Source Ring MB | Transcoder Ring MB | TsMux Ring MB | AVIO HWM MB |
|---|---:|---:|---:|---:|
| Empty baseline | 0.1 | 0.0 | 0.0 | 0.0 |
| Same ingest growth, 5x H.264 SRT | 19.0 | 0.0 | 0.0 | 0.0 |
| Mixed ingest growth, 5 ingest types | 15.6 | 0.0 | 0.0 | 0.0 |
| Mixed source egress, 20 outputs | 5.8 | 0.0 | 1.5 | 0.5 |
| Mixed 720p transcode egress, 20 outputs | 5.7 | 8.3 | 4.3 | 4.6 |
| HEVC bridge, 10 RTMP source outputs | 5.8 | 8.2 | 0.0 | 0.0 |

Takeaways:

- Idle baseline is about `73 MB` in the Restream process before live traffic.
- Ingest fan-in without transcode is cheap in RSS: `~76-83 MB` for five live
  pipelines depending on mix.
- Mixed `720p` transcode egress is the main external-process memory consumer:
  `~120 MB` in Restream plus `~166 MB` in the child FFmpeg at 20 outputs.
- HEVC bridge remains expensive in-process: `~159 MB` and `~72%` CPU at 10
  source outputs, with no external child involved.

### Bitrate Sweep

Bitrate sweep runs one pipeline with four outputs (`RTMP source`, `RTMP 720p`,
`SRT source`, `SRT 720p`) and verifies all four with `ffprobe`.

| Ingest Config | Bitrate | Restream MB | Child FFmpeg MB | Combined MB | Restream CPU % | Child FFmpeg CPU % | Total CPU % | Correctness |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| `h264-rtmp` | 1.5M | 86.9 | 167.0 | 253.9 | 7.63 | 33.49 | 41.12 | PASS |
| `h264-rtmp` | 4M | 103.2 | 166.9 | 270.1 | 6.43 | 33.13 | 39.56 | PASS |
| `h264-rtmp` | 8M | 118.2 | 169.2 | 287.3 | 5.69 | 31.99 | 37.68 | PASS |
| `h264-srt` | 1.5M | 93.7 | 166.5 | 260.2 | 7.16 | 34.79 | 41.95 | PASS |
| `h264-srt` | 4M | 112.3 | 167.4 | 279.7 | 7.76 | 37.85 | 45.61 | PASS |
| `h264-srt` | 8M | 136.7 | 160.8 | 297.5 | 7.66 | 36.76 | 44.42 | PASS |
| `h265-srt` | 1.5M | 220.1 | 317.0 | 537.1 | 173.84 | 256.39 | 430.23 | PASS |
| `h265-srt` | 4M | 241.8 | 299.7 | 541.5 | 146.59 | 160.12 | 306.71 | PASS |
| `h265-srt` | 8M | 278.4 | 303.3 | 581.6 | 161.90 | 148.84 | 310.75 | PASS |
| `h264-srt-multi` | 1.5M | 93.8 | 167.4 | 261.2 | 7.89 | 38.91 | 46.80 | PASS |
| `h264-srt-multi` | 4M | 111.2 | 168.1 | 279.3 | 8.19 | 39.01 | 47.20 | PASS |
| `h264-srt-multi` | 8M | 135.1 | 170.3 | 305.4 | 8.99 | 37.95 | 46.94 | PASS |
| `h265-srt-multi` | 1.5M | 215.8 | 317.9 | 533.7 | 168.13 | 243.90 | 412.03 | PASS |
| `h265-srt-multi` | 4M | 240.8 | 300.1 | 541.0 | 125.86 | 141.00 | 266.86 | PASS |
| `h265-srt-multi` | 8M | 252.0 | 316.7 | 568.6 | 160.18 | 159.96 | 320.14 | PASS |

Current bitrate-sweep takeaways:

- H.264 ingest scales upward with bitrate mostly in retained memory, not in a
  proportional jump in CPU. Combined memory ends up in the `~254-305 MB` range
  for the four-output shape.
- External FFmpeg RSS is comparatively flat for H.264 cases, roughly
  `161-170 MB`, while Restream parent RSS grows with bitrate and protocol mix.
- H.265 ingest is much more expensive because the bridge/transcode path is
  active. Combined memory is `~534-582 MB`, and total CPU is `~267-430%`
  depending on bitrate and audio shape.
- All 15 current cases passed output correctness.
## Media Correctness Findings (2026-07-01)

These issues were found while hardening the `mixed.live.srt.h265.a2.bf2` live matrix
around the checked-in H.265 + two-audio fixture.

### Fixed Runtime Issues

- RTMP egress could emit equal or backward timestamps when source packets had
  repeated millisecond DTS/PTS. Runtime now guards RTMP video and audio
  timestamps independently, and unit tests cover repeated video DTS, repeated
  audio PTS, and A/V stream independence.
- MPEG-TS muxing could emit equal DTS when packet timestamps repeated at
  millisecond precision. The muxer now enforces strictly increasing 90 kHz DTS
  per elementary stream, with unit coverage for repeated timestamps and
  independent audio tracks.
- SRT selected-track egress could advertise ingest audio tracks that were not
  present in the routed output ring. The shared TS muxer now prefers routed
  `RingBuffer::audio_tracks()` metadata when available, and the regression test
  verifies the PMT contains only the selected audio track.
- ADTS audio payloads can contain multiple AAC frames inside one PES. Treating
  only the PES start timestamp as occupied allowed the next PES to collide with
  the final internal AAC frame after FFprobe split the frames. The muxer now
  reserves the full ADTS frame span before accepting the next DTS. Unit coverage
  includes deterministic multi-frame AAC and a property test for ADTS frame
  counting.
- RTMP egress wrapped Raw Annex B H.264 as FLV/AVCC with composition time `0`.
  B-frame fixtures therefore lost their `PTS-DTS` offset on RTMP output and the
  mixed-file multi live row exposed downstream duplicate/non-monotonic DTS
  warnings. The Raw H.264 RTMP wrapper now preserves signed 24-bit FLV
  composition time, with unit coverage for positive and negative offsets.

### Validator Lessons

- MediaMTX remains valuable as an interoperability sink, but it is not the only
  correctness oracle. Direct `ffprobe`/`ffmpeg` sinks are required when debugging
  muxer-level timestamp failures.
- MediaMTX SRT readback reproduced non-monotonic DTS with Restream bypassed in
  a direct FFmpeg-to-MediaMTX control. That specific path is therefore treated
  as a compatibility/readback signal, not strict proof of Restream muxer output.
- FFmpeg decode-to-`null` can introduce muxer-layer DTS warnings after decode,
  especially with multi-audio PCM output. The direct SRT sink now uses
  `ffprobe` compact packet output for stream shape and packet timestamp checks,
  avoiding false positives from a newly-created output muxer.
- FFprobe packet dumps may print elementary streams in demuxer flush order, not
  raw physical TS packet order. The harness validates duplicate DTS and large
  per-stream gaps after sorting each stream's timestamps instead of requiring
  the printed order to be monotonic.

### Required Controls

- Probe the checked-in fixture before blaming Restream:
  `ffprobe -v warning -show_entries program=:stream=index,codec_type,width,height:packet=stream_index,dts_time,pts_time -of compact=p=1:nk=0 test/fixtures/transport/bench-h265-1_5m-2a.ts`.
- For sink disputes, run FFmpeg/FFprobe directly against the sink path with
  Restream bypassed. If the control reproduces the warning, keep MediaMTX in
  the matrix for interoperability but use a direct FFmpeg-family sink for muxer
  correctness.
- The direct SRT correctness mode is `SRT_SINK=ffmpeg` on
  `mixed.live.srt.h265.a2.bf2`; it validates stream dimensions, selected audio-track
  count, duplicate DTS, large DTS gaps, and FFmpeg-family probe warnings.
