# Testing

This is the current verification guide. Start with the smallest gate that can
prove the changed behavior, then broaden according to the affected boundary.
The accepted tiering rationale lives in the
[testing decision record](testing-strategy.md); current commands and policies
live here and in `AGENTS.md`.

## Contents

- [Rust test suite](#rust-test-suite)
- [Frontend test split](#frontend-test-split)
- [Parallelism policy](#parallelism-policy)
- [Scoped verification loop](#scoped-verification-loop)
- [Evidence and generated inventories](#evidence-and-generated-inventories)
- [Live integration tests](#live-integration-tests)
- [Capability gates](#capability-gates)

## Rust test suite

Run the repo gate:

```sh
./scripts/check/test-hygiene.sh
```

For fixture-first media discipline:

```sh
./scripts/check/fixture-discipline.sh
```

For a plain full-suite run without the hygiene scan:

```sh
scripts/build/resource-limit.sh cargo test
```

Keep successful logs quiet. New tests should not land with compiler warnings,
panic text, FFmpeg probe chatter, or similar “expected noise” in passing runs;
fix or suppress that output at the helper level instead.

## Frontend test split

Frontend confidence is intentionally split between TypeScript ownership and
compiled-bundle smoke coverage:

- `npm run test:frontend` runs the Node-based frontend suites from a temporary
  sourcemapped build of `web/ts/**`, then finishes with a smaller smoke pass
  against the shipped `public/js/**` bundle.
- `npm run test:frontend:coverage` keeps the same split, but reports coverage
  back onto the deterministic TypeScript modules that the Node/fake-DOM suite
  is meant to own. This is the main frontend coverage gate. Runtime transport
  modules such as `features/dashboard.ts`, `features/modes.ts`,
  `features/status.ts`, `features/publisher-health.ts`, and
  `history/controller.ts` are part of this covered surface.
- `npm run test:frontend:coverage:all` keeps the same runtime path but emits a
  broader all-files TypeScript report for diagnostic use; expect browser-heavy
  modules to stay lower until they get Playwright or browser-native coverage.
- `npm run test:frontend:js-smoke` is the minimal direct guard for generated
  `public/js/**`; use it when you only need to verify the compiled artifact.

This keeps detailed behavior and coverage attached to the TypeScript source of
truth without dropping confidence in the emitted browser bundle, while avoiding
misleading Node-only coverage targets for browser-heavy modules.

### Layered UI strategy

Treat frontend confidence as four layers, each owning a different kind of risk:

| Layer | Purpose | Typical command |
|---|---|---|
| TypeScript/source logic | Keep parsing, helpers, API choke points, and pure UI state logic deterministic and cheap. | `npm run test:frontend` |
| Fake-DOM scenario matrices | Replace repetitive manual "check every state" work for state-heavy renderers. | `npm run test:frontend` |
| Browser-native DOM checks | Prove real DOM events, focus/ARIA behavior, overlay positioning hooks, and browser-only widget behavior without starting the full Rust app. | `npm run test:frontend:browser-dom` |
| Full app/browser integration | Prove login, navigation, media playback, real network wiring, and end-to-end runtime behavior against an isolated app with committed fixtures. | `npm run test:e2e` |

`npm run test:e2e` is self-contained: it builds the native-linked debug app and
frontend, seeds the required checked-in multi-audio fixture into an isolated
`.local/e2e/` media directory, waits for `/healthz`, runs Playwright, and
stops only the app process it started. Do not manually start a dashboard before
using it.

Use the lowest layer that can actually catch the bug. Move upward only when the
lower layer cannot prove the behavior.

For the native fMP4 preview path specifically:

- `scripts/build/resource-limit.sh cargo test hls_fmp4 -- --nocapture` covers the unit,
  proptest, and loom-backed correctness checks for rendition publication and
  sample timestamp packaging.
- `scripts/build/resource-limit.sh cargo bench --profile bench --bench hls_fmp4_cost`
  measures fMP4 segment muxing plus the multi-rendition in-memory publication
  path used by browser preview.
- `npm run test:frontend:browser-dom` keeps the preview audio-track picker
  behavior deterministic, and `npx playwright test test/frontend/hls-player.spec.ts`
  proves the full browser flow against the running app, including real video
  load and alternate-audio selection.

### UI scenario matrices

When a dashboard surface starts accumulating too many manual "click every state"
checks, add a fake-DOM scenario matrix instead of growing Playwright coverage
for every badge and branch.

- Use `test/support/helpers/ui-scenario-harness.mjs` to mount the minimum DOM, load the
  compiled frontend module, and run a named state matrix under `npm run test:frontend`.
- Current examples:
  `test/frontend/frontend-output-scenarios.test.mjs` and
  `test/frontend/frontend-pipeline-info-scenarios.test.mjs`.
- Feed renderers a bounded set of important states such as healthy, retrying,
  flapping, stalled, stopped, long text, and missing optional metadata.
- Assert operator-visible structure and state: the right action label,
  warning/error affordance, hidden/visible controls, and critical metrics.
- Keep browser-native checks in Playwright for things the fake DOM cannot prove:
  navigation, focus, media playback, sizing, and real browser APIs.
- Use `npm run test:frontend:browser-dom` for a self-contained browser-native
  slice that serves the compiled frontend assets from a lightweight local static
  server instead of requiring the full Rust dashboard app to be started first.

Use `cargo test -- --list` when a current test inventory is needed; do not copy
the resulting count into maintained documentation.

Checked-in fixture contracts now cover the committed benchmark/test media under
`test/fixtures/transport/`, so the transcoder and fixture-dependent suites no longer rely
on ad-hoc local artifacts. Tests, benches, and harness publishers should resolve
those assets through `src/test_fixtures.rs` so missing files fail loudly and new
fixtures are added to one explicit contract.

Historical architecture-regression artifacts are indexed in
[`regression-artifacts.md`](regression-artifacts.md). The index maps each known
failure class to its durable fixture, harness replay command, generated-artifact
location, or proof gate; generated `.local/artifacts/` run directories remain
uncommitted.
## Parallelism policy

Keep correctness throughput high, but treat measurement fidelity as a separate
constraint.

- Rust unit and integration tests: prefer a single `scripts/build/resource-limit.sh cargo test ...`
  invocation and let Cargo own compile parallelism while `resource-limit.sh`
  bounds `RUST_TEST_THREADS` from the same available-memory and CPU budget.
  Avoid launching multiple heavy `cargo test` commands against the same worktree
  at once; that just trades useful concurrency for lock contention and noisier
  logs.

### Test thread concurrency

`scripts/build/resource-limit.sh` derives a default `RUST_TEST_THREADS` from
available memory so that `cargo test` does not exhaust RAM on constrained
machines such as WSL2 or small CI runners.

The default budget is 500 MB per test thread (`RESTREAM_MB_PER_TEST_THREAD`,
minimum 1). The final value is capped so that test threads never exceed the
available CPU count minus the configured reserve
(`RESTREAM_CPU_RESERVE`, default 1).

```sh
# Let the wrapper derive the thread count from the machine's resources:
scripts/build/resource-limit.sh cargo test

# Override the per-thread memory budget:
RESTREAM_MB_PER_TEST_THREAD=1024 scripts/build/resource-limit.sh cargo test

# Pin an explicit thread count (skips memory derivation entirely):
RUST_TEST_THREADS=2 scripts/build/resource-limit.sh cargo test
```

The derivation runs both on the outer lock-acquiring invocation and on nested
(re-entrant) invocations, so the budget is always applied when the build lock
is held.
- Live harness correctness modes: `src/bin/test_harness.rs` may batch
  correctness-only suite modes in parallel when each mode is isolated in its
  own network namespace and work directory.
- Measurement-oriented harness modes: keep them serial and bench-profile only.
  CPU, RSS, and throughput numbers are only comparable when the harness runs one
  measurement slice at a time from `target/bench/`.
- Criterion benches: parallelize compilation and fixture preparation, not timed
  measurement. `scripts/build/resource-limit.sh cargo bench --no-run` is the safe fan-out
  step; actual `cargo bench --bench ...` execution should stay serial unless the
  runs are explicitly resource-isolated.
## Scoped verification loop

Prefer the smallest test and benchmark set that directly covers the changed
behavior, then broaden only when the risk calls for it. This keeps agent and
developer loops fast while still making the verification signal precise.

Good scoped Rust patterns:

```sh
scripts/build/resource-limit.sh cargo test --lib <test-name-or-module-filter>
scripts/build/resource-limit.sh cargo test --test api <test-name-filter>
scripts/build/resource-limit.sh cargo test --test transcoder <test-name-filter>
```

Good scoped benchmark patterns:

```sh
scripts/build/resource-limit.sh cargo bench --bench <bench-name> -- <criterion-filter>
scripts/build/resource-limit.sh cargo bench --bench high_performance_data_path -- data_path/egress_progress
scripts/build/resource-limit.sh cargo bench --bench srt_ingest_latency -- 'srt_(ingest|egress)'
scripts/build/resource-limit.sh cargo bench --bench hls_fmp4_cost -- hls_fmp4_cost
```

The SRT bench is a socket-pair microbenchmark, not a live pipeline test. It is
meant to answer narrow questions such as "what did enabling SRT encryption cost
on loopback?" by comparing:

- `srt_ingest/plain|aes128|aes192|aes256/recv_path`
- `srt_egress/plain|aes128|aes192|aes256/send_path`

Each case uses the same fixed transfer shape: `8` live-mode SRT packets of
`1316` bytes per timed iteration. The only benchmark variable is the negotiated
SRT encryption key length through `SRTO_PBKEYLEN`.

Use the full `cargo test` suite, full benchmark suites, or live integration
modes as a broader confidence pass when a change crosses module boundaries,
changes a shared contract, affects protocol behavior, or touches a hot path
whose blast radius is unclear. If an unrelated full-suite test or benchmark
fails, report it separately from the scoped signal for the current change.

### Composable verification stages

Large suites should be broken into named stages that can run independently and
compose into larger gates. A failure in one stage should identify the affected
behavior slice instead of turning the entire test or benchmark program into an
opaque blocker.

| Stage | Purpose | Typical commands |
|---|---|---|
| 0. Preflight/static | Prove the environment and cheap invariants before spending runtime. | `cargo fmt --all --check`, integration `--preflight` |
| 1. Changed behavior | Fastest proof for the exact code path touched by a change. | `cargo test --lib <filter>`, `cargo test --test api <filter>` |
| 2. Contract slice | Neighboring API, graph, stage, protocol, or lifecycle contracts that consume the changed behavior. | Filtered package/integration tests by module, endpoint, protocol, or stage kind |
| 3. Hot-path cost | Criterion group that measures the touched hot path only. | `cargo bench --bench <bench> -- <criterion-filter>` |
| 4. Live protocol slice | One live protocol/topology check with minimal fanout and targeted assertions. | `target/bench/test_harness mixed.live.srt.h264.a1.bf2` |
| 5. Scale/degradation slice | A bounded load, ramp, restart, queue-pressure, or bonding slice for resource shape. | `N_OUTPUTS=<small>` ramp, `N_PER_GROUP=<small>` mixed.matrix, `bonding` |
| 6. Full confidence gate | Release or milestone pass assembled from the relevant stages above. | Full `cargo test`, selected full benches, full integration modes |

When a suite grows too large, split it along composable axes instead of adding
more mandatory work to a single command:

- behavior: ingest, egress, HLS, recording, graph, diagnostics, alerts
- protocol: RTMP, SRT, HLS, RTMPS, SRT bonding
- codec/media shape: H.264, H.265, B-frames, multi-audio, audio remap/downmix
- topology: passthrough, one shared stage, mixed presets, package sharing
- load shape: smoke, small fanout, ramp, soak, downstream restart, queue pressure
- evidence: unit assertion, API snapshot, graph invariant, ffprobe/readback,
  resource baseline, Criterion benchmark

Prefer adding selectors, manifest entries, and result artifacts over adding a
new all-or-nothing suite. A milestone can still require multiple stages, but it
should state which slices are required and preserve each slice's separate
pass/fail result.

Unit coverage includes:

- RTMP FLV H.264/AAC parsing and signed composition time
- HLS playlist/window behavior
- SRT stream-ID normalization, URL/bond parsing, codec mapping, payload
  extraction, rate deltas, socket option IDs, listener UDP-stat parsing
- Linux `TCP_INFO`/`SO_MEMINFO` conversion and live socket collection
- Transcoder stage sharing and audio-routing parsing
- External HLS PUT upload delivery through a dummy HTTP sink
- FFmpeg-backed audio remap/downmix stage argument generation and fixture-backed
  execution
- Internal decode/scale/encode coverage for the built-in video profiles
- Ring buffer push/pull ordering, overflow fast-forward to keyframe,
  multi-reader isolation, fill/capacity reporting, burst APIs
- Multi-input gate state/property/loom coverage plus bounded latest-GOP
  retention, overflow invalidation, replay ordering, and repeated timestamp
  rebasing
- DTS monotonicity enforcement (equal, decreasing, PTS < DTS correction,
  per-stream independence, B-frame composition-time preservation)
- Engine lifecycle: ingest/egress register/unregister/cancel, idempotent
  unregister, pipeline create/remove, egress byte counters, health snapshot
  pipeline filtering, recording lifecycle, noop on nonexistent pipelines
- MPEG-TS demux/mux: packet parsing, PID dispatch, PES assembly, continuity
  counters, Annex-B NAL scanning, vectorized resync
- Codec helpers: FLV stripping, video/audio payload conversion for TsMuxer

The API suite covers authentication, configuration, pipeline/output
CRUD, ingests, HLS aliases, status, graph, diagnostics preconditions, custom
encoding persistence/rejection for runtime outputs, HLS upload output
acceptance, RTMPS output acceptance, egress-pipeline association in `/api/v1/engine/health`,
deletion-cancellation of egress tasks, media list / analysis / rename / delete
behavior, pipeline and aggregate alerts response shape, system metrics
structured response, agent graph-diff-preview compiled-out behavior, and
operator telemetry/events/overview/summary endpoints.
## Evidence and generated inventories

Maintained testing guidance intentionally does not copy route totals, test
counts, coverage percentages, or resource snapshots. Use the owning source or
generated evidence instead:

- routes: `PUBLIC_ROUTE_PATHS` and `AUTHENTICATED_ROUTE_PATHS` in
  `src/api/router.rs`, checked by `tests/api.rs`;
- Rust tests: `cargo test -- --list` and the test runner output;
- coverage: `npm run test:frontend:coverage` and the repository coverage
  workflow/artifacts;
- performance and resource evidence: the dated
  [quality baseline ledger](agent-guidance/quality/baselines.md) and CI
  artifacts produced by the owning workflow.

## Live integration tests

The checked-in manifest catalog under `test/harness/` is the command and
workflow source of truth. Do not copy its modes or catalog subcommands into
this guide. Prepare the current bench-profile harness and ask the binary for
its catalog usage:

```sh
scripts/harness/run.sh --prepare
target/bench/test_harness catalog help
```

Run a mode through the wrapper so stale binaries are rebuilt and the shared
build lock is respected:

```sh
scripts/harness/run.sh <mode>
scripts/harness/run.sh <mode> -- --no-netns
```

Use `MIXED_OUTPUT_GROUPS` only for focused live proofs that need a subset of a
mixed output matrix, for example a codec-edge smoke that should exercise
`rtmp.720p.a0,rtmp.720p.a1` without paying for every SRT and RTMP row. The value
is a comma-separated list of mixed output row ids; broad coverage still belongs
to the catalog matrix, fast-breadth, and signal modes.

Integration tests use a private loopback namespace by default. Use
`--no-netns` only when the host cannot create the namespace or the test must
interact with a host service. Never build while Restream, MediaMTX, or FFmpeg
live-pipeline processes are running.

### Choosing a live proof

Choose the narrowest catalog mode that crosses the changed boundary:

- protocol or codec behavior — one matching mixed RTMP/SRT scenario;
- multi-input standby cost — `msr-smoke` in nightly or a selected
  `mixed.fast-breadth` RTMP/SRT sentinel;
- multi-input promotion — `fault.resilience`, whose RTMP/SRT cases use a
  10-second GOP and require cached replay progress within five seconds;
- teardown or recovery — the matching fault workflow;
- HLS, recording, or file ingest — a scenario whose resolved plan includes the
  relevant sink and checks;
- broad regression confidence — a catalog suite after the scoped mode passes;
- performance or capacity — a measurement workflow, run serially with
  bench-profile binaries.

Use the catalog's current inspection commands to resolve a mode and review its
services, scenarios, checks, timeouts, and artifacts before spending time on a
live run.

### Fixtures and artifacts

Harness publishers and probes must resolve committed media through
`src/test_fixtures.rs`. Add new assets to the checked-in fixture contract; do
not generate substitute media inline for a passing test.

Each run writes under `.local/artifacts/` using its run identity. Preserve the
manifest, result stream, logs, probe output, and failure snapshots needed to
explain the verdict. Generated run directories are evidence, not source, and
must not be committed.

### Correctness and measurement remain separate

Correctness modes answer whether protocols, timestamps, stream selection,
lifecycle, and recovery satisfy their contracts. Measurement modes answer how
much CPU, memory, latency, or throughput a known-correct path consumes. Do not
weaken correctness checks to make a measurement pass, and do not use debug
binaries for resource or performance conclusions.

For the rationale behind this split, see
[testing-strategy.md](testing-strategy.md). For protocol-specific execution,
use the canonical protocol-test skill or inspect the relevant catalog plan.

## Capability gates

These capabilities must be treated as test results, not assumptions:

| Capability | Gate |
|---|---|
| RTMP H.264/AAC ingest and egress | B-frame timestamp round-trip through `target/debug/test_harness timestamp.bframe` |
| SRT H.264 and H.265 ingest/egress | Full correctness matrix |
| H.265 SRT passthrough | Live HEVC identity preservation through `target/debug/test_harness mixed.live.srt.h265.a1.bf2` |
| H.265 source to RTMP egress | Live H.265→H.264 edge conversion through `target/debug/test_harness mixed.live.srt.h265.a1.bf2` |
| Cross-protocol SRT→RTMP | Live H.264/AAC packetization through `target/debug/test_harness mixed.live.srt.h264.a1.bf0` |
| Built-in video presets (`h264`, `720p`, `1080p`) | Decode/filter/encode loop is covered by transcoder integration tests |
| Additional/custom video presets | Must be explicitly profiled and matrix-tested before advertising |
| Embedded FFmpeg subprocess feature set | `scripts/build/app-static.sh` runs `restream-ffmpeg-capabilities` to prove the required codecs, `file`/`pipe` protocols, and `mov`/`matroska`/`mpegts` mux/demux surface are present |
| HLS live segments | Native TsMuxer validates in-memory |
| HLS upload egress | YouTube-style `file=` and path-style signed-query HTTP PUT delivery plus destination restart recovery are covered by unit tests and the `mixed.live.srt.h264.a1.bf2` HLS PUT probe |
| Recording | Readable file with correct streams/timestamps |
| Audio remap/downmix | Channel-level filtering is implemented for the default runtime; full audio-content matrix remains required |
| Custom encoding | Runtime output selection must stay rejected until custom args are applied by a transcoder backend |
| Bonded SRT ingest | Separate-process broadcast + backup tests |
