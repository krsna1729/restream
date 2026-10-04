# Testing

This is the current verification guide. Start with the smallest gate that can
prove the changed behavior, then broaden according to the affected boundary.
The accepted tiering rationale lives in the
[testing decision record](#why-two-test-tiers); current commands and policies
live here and in `AGENTS.md`.

## Contents

- [Rust test suite](#rust-test-suite)
- [Frontend test split](#frontend-test-split)
- [Parallelism policy](#parallelism-policy)
- [Scoped verification loop](#scoped-verification-loop)
- [Evidence and generated inventories](#evidence-and-generated-inventories)
- [Live integration tests](#live-integration-tests)
- [Capability gates](#capability-gates)
- [Container runtime smoke](#container-runtime-smoke)
- [SRT egress qualification](#srt-egress-qualification)
- [Why two test tiers](#why-two-test-tiers)
- [Stage boundary proof map](#stage-boundary-proof-map)
- [Frontend boundary proof map](#frontend-boundary-proof-map)
- [Regression artifacts](#regression-artifacts)

## Rust test suite

Run the repo gate:

```sh
cargo xtask test-hygiene
```

For fixture-first media discipline:

```sh
cargo xtask fixture-discipline
```

For a plain full-suite run without the hygiene scan:

```sh
cargo test
```

Keep successful logs quiet. New tests should not land with compiler warnings,
panic text, FFmpeg probe chatter, or similar “expected noise” in passing runs;
fix or suppress that output at the helper level instead.

### Parser fuzz targets

`fuzz/` holds cargo-fuzz targets for the parsers that read untrusted media
bytes. They call the production functions; RTMP-private parsers are reached
through `media::rtmp::fuzz_entry`, which exists only under `--cfg fuzzing`.

| Target | Input | Production code |
|---|---|---|
| `flv_video_tag` | RTMP ingest video tag | `classify_flv_video_packet`, `flv_avcc_config_annexb_parameter_sets`, `parse_flv_video_meta` (SPS) |
| `flv_audio_tag` | RTMP ingest audio tag | `parse_flv_audio_meta` (AudioSpecificConfig) |
| `avcc_annexb` | H.264 from either ingest | `parse_avcc_config`, `avcc_to_annexb`, `annexb_to_avcc`, `build_avcc_sequence_header` (must read back) |
| `hevc_enhanced_rtmp` | HEVC Annex-B from SRT ingest | `build_hevc_enhanced_rtmp_sequence_header` (SPS profile/tier/level), coded-frame packer |
| `rtmp_server_responses` | destination server bytes after the handshake | `RtmpSessionCore::handle_server_input` |
| `rtmp_client_requests` | publisher bytes after the handshake | ingest `ServerSession::handle_input` with the message-size limit |
| `ts_demux` | SRT ingest MPEG-TS, sync-forced or raw | `TsDemuxer` (PAT/PMT, PES, `mpegts_probe`) |

```sh
cd fuzz
./seed-corpus.sh   # seeds ts_demux from the checked-in TS fixtures
cargo +nightly fuzz run <target> -- -max_total_time=60
```

CI (`Parser fuzz smoke`) runs every target for 30 s. A crash becomes a
regression unit test next to the parser before the fix lands.

## Frontend test split

Frontend confidence is intentionally split between TypeScript ownership and
compiled-bundle smoke coverage. Current invariant coverage by UI contract
boundary — SSE reconnects, mutation convergence, auth/session, route
ownership, accessible structure — is tracked in
[frontend boundary proof map](#frontend-boundary-proof-map), the
frontend counterpart to [stage boundary proof map](#stage-boundary-proof-map):

- `npm run test:frontend` runs the Node-based frontend suites from a temporary
  sourcemapped build of `web/ts/**`, then finishes with a smaller smoke pass
  against the shipped `public/js/**` bundle.
- `npm run test:frontend:coverage` keeps the same split, but reports coverage
  back onto the deterministic TypeScript modules that the Node/fake-DOM suite
  is meant to own. This is the main frontend coverage report, run on every PR
  by the `frontend` CI job — advisory, not threshold-gated, matching the
  backend `coverage` job's posture (`cargo llvm-cov` also reports every PR
  without failing the build on a percentage). Neither stack fails CI on a
  coverage number; both make the trend visible. Runtime transport modules
  such as `features/dashboard.ts`, `features/modes.ts`,
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

`scripts/dev/frontend/node-tests.sh`'s `NODE_COVERAGE_EXCLUDES` is
deliberately short: only modules Node's fake-DOM harness genuinely cannot
exercise belong there (`app/dashboard-entry.ts`, a side-effecting bootstrap
entry point; `features/hls-player.ts` and `features/input-preview.ts`, real
`<video>`/`<audio>`/hls.js element wiring, covered instead by
`test/frontend/hls-player.spec.ts`, `test/frontend/frontend-browser-dom.spec.ts`,
and `test/frontend/redesign/seed-scale.spec.ts`). Use
`npm run test:frontend:coverage:all` to check whether a module belongs on
this list before adding to it: if it already shows non-trivial coverage
there, Node is exercising it fine.

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

- `cargo test hls_fmp4 -- --nocapture` covers the unit,
  proptest, and loom-backed correctness checks for rendition publication and
  sample timestamp packaging.
- `cargo bench --profile bench --bench hls_fmp4_cost`
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

### Property and interleaving tests

Hand-picked scenario fixtures prove the cases someone thought to write down;
they miss the cases nobody thought of. For pure state-derivation logic and
for closures that guard against stale-callback races (the frontend analog of
a wait/cancel/reconnect boundary), add a [fast-check](https://github.com/dubzzz/fast-check)
property test instead of another hand-picked case:

- `test/frontend/pipeline-output-overview.property.test.mjs` generates
  randomized output fleets and checks structural invariants of
  `buildPipelineOutputOverviewModel` (bucket partitioning, attention-list
  bounds and tone, card pagination, order preservation) — the frontend
  equivalent of a backend `proptest!` suite.
- `test/frontend/frontend-log-stream-interleaving.property.test.mjs` replays
  randomized interleavings of `sync()` calls and `EventSource` events
  (including events from a source the module has already superseded)
  against `core/log-stream.ts`, and asserts its staleness guard
  (`source !== openedSource`) never lets a stale event reach `onLog` — the
  frontend equivalent of a loom model check for a wait/reconnect race.

When adding one: verify it actually catches a regression before trusting
it — temporarily break the invariant it targets, confirm the property goes
red with a shrunk counterexample, then revert. A property test that never
fails on a real bug is not proof.

### Specs excluded from the default Playwright run

`test/frontend/msr-dashboard-soak.spec.ts` is gated behind
`MSR_DASHBOARD_PLAYWRIGHT` (excluded via `playwright.config.ts`'s
`testIgnore`) and defaults to a 30-minute churn/soak run against real
pipelines and outputs — intentionally too heavy for per-PR CI. Run it
manually, or from a future nightly slice, rather than on every PR.

`test/frontend/redesign/visual-accessibility.spec.ts` (axe-core a11y +
keyboard/ARIA/contrast checks) used to be excluded the same way but is now
part of the default suite, run by the `playwright` CI job. Getting it there
required fixing `getCdpHeadingLevels()`: it was walking
`Accessibility.getFullAXTree()`'s flat node array in its raw (internal
computation) order instead of via `parentId`/`childIds`, so heading-order
assertions saw an arbitrary order instead of true reading order — not a real
accessibility regression in the dashboard. It also had no committed
screenshot/ARIA-snapshot baselines; those are now generated and checked in
from the `playwright` CI job's own render (local Chromium/fontconfig differs
enough from the CI runner's to make locally-generated screenshots and
sub-pixel-sensitive width checks unreliable there — generate/update these
baselines from CI, not a workstation). A first CI run also found the
touch-target-size check was sweeping in daisyUI's `.btn-xs` compact buttons
(media library row actions like Rename/Delete/Play/Download); those are
deliberately small, not primary affordances, so the check now excludes them.

Use `cargo test -- --list` when a current test inventory is needed; do not copy
the resulting count into maintained documentation.

Checked-in fixture contracts now cover the committed benchmark/test media under
`test/fixtures/transport/`, so the transcoder and fixture-dependent suites no longer rely
on ad-hoc local artifacts. Tests, benches, and harness publishers should resolve
those assets through `src/test_fixtures.rs` so missing files fail loudly and new
fixtures are added to one explicit contract.

Historical architecture-regression artifacts are indexed in
[regression artifacts](#regression-artifacts). The index maps each known
failure class to its durable fixture, harness replay command, generated-artifact
location, or proof gate; generated `.local/artifacts/` run directories remain
uncommitted.
## Parallelism policy

Keep correctness throughput high, but treat measurement fidelity as a separate
constraint.

- Rust unit and integration tests: prefer a single `cargo test ...`
  invocation. Cargo owns compile parallelism (`.cargo/config.toml`,
  `jobs = -1`) and libtest picks its default thread count. Avoid launching
  multiple heavy `cargo test` commands against the same worktree at once;
  that just trades useful concurrency for lock contention and noisier logs.
  On a memory-constrained host, pin `RUST_TEST_THREADS=N` and
  `CARGO_BUILD_JOBS=N` explicitly.
- Live harness correctness modes: `src/bin/test_harness.rs` may batch
  correctness-only suite modes in parallel when each mode is isolated in its
  own network namespace and work directory.
- Measurement-oriented harness modes: keep them serial and bench-profile only.
  CPU, RSS, and throughput numbers are only comparable when the harness runs one
  measurement slice at a time from `target/bench/`.
- Criterion benches: parallelize compilation and fixture preparation, not timed
  measurement. `cargo bench --no-run` is the safe fan-out
  step; actual `cargo bench --bench ...` execution should stay serial unless the
  runs are explicitly resource-isolated.
## Scoped verification loop

Prefer the smallest test and benchmark set that directly covers the changed
behavior, then broaden only when the risk calls for it. This keeps agent and
developer loops fast while still making the verification signal precise.

Good scoped Rust patterns:

```sh
cargo test --lib <test-name-or-module-filter>
cargo test --test api <test-name-filter>
cargo test --test transcoder <test-name-filter>
```

Good scoped benchmark patterns:

```sh
cargo bench --bench <bench-name> -- <criterion-filter>
cargo bench --bench high_performance_data_path -- data_path/egress_progress
cargo bench --bench srt_ingest_latency -- 'srt_(ingest|egress)'
cargo bench --bench hls_fmp4_cost -- hls_fmp4_cost
```

The SRT bench is a socket-pair microbenchmark, not a live pipeline test. It is
meant to answer narrow questions such as "what did enabling SRT encryption cost
on loopback?" by comparing:

- `srt_ingest/plain|aes128|aes192|aes256/recv_path`
- `srt_egress/plain|aes128|aes192|aes256/send_path`

Each case uses the same fixed transfer shape: `8` live-mode SRT packets of
`1316` bytes per timed iteration. The only benchmark variable is the negotiated
SRT encryption key length as selected by the `srt-proto` key-length setting.

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
- performance and resource evidence: the commit message of the measured
  change and CI artifacts produced by the owning workflow.

## Live integration tests

The checked-in manifest catalog under `test/harness/` is the command and
workflow source of truth. Do not copy its modes or catalog subcommands into
this guide. Prepare the current bench-profile harness and ask the binary for
its catalog usage:

```sh
scripts/harness/run.sh --prepare
target/bench/test_harness catalog help
```

Run a mode through the wrapper so stale binaries are rebuilt:

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

### Transport live-CI tiers

- Pull requests run one short live SRT smoke and one RTMP-input smoke. The RTMP
  mixed matrix includes its RTMPS output row; a capability preflight reports
  missing Linux kTLS support as an explicit failure rather than falling back
  to userspace TLS. The fast concurrency proof stays on pull requests; full
  live lifecycle faults are deferred to the `master` push tier.
- Pushes to `master` run the H.264/H.265 SRT live shapes, two RTMP live
  shapes, `srt-crypto-matrix`, and `fault.resilience`. The concurrency live
  lifecycle job adds `fault.egress-retry`, `fault.output-stall`, and `recovery`.
  `fault.resilience` also proves RTMPS media delivery, sink loss reaching
  `retrying`, and successful SIGTERM shutdown with
  `restream.shutdown.completed` within the five-second bound.
- Nightly certification adds the broader file/live, crypto, bitrate, ramp,
  resource, and churn matrix. Its harness artifacts upload on every result;
  pull-request failure/cancellation artifacts are retained for 3 days and
  `master` failure/cancellation artifacts for 7 days. The `master` push
  concurrency fault logs/results are retained for 7 days on failure/cancel.

The catalog and workflow matrices are the source of truth for exact shard
names. A capability failure is evidence that the runner cannot qualify the
requested transport; do not silently skip or substitute a different transport.
RTMPS ingest is not in the current product contract; its live leg is RTMP
ingest → Restream → RTMPS egress.

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
[why two test tiers](#why-two-test-tiers). For protocol-specific execution,
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
| Linked FFmpeg library feature set | `scripts/build/app-static.sh` runs `restream-ffmpeg-capabilities` to prove the required codecs, `file`/`pipe` protocols, and `mov`/`matroska`/`mpegts` mux/demux surface are present |
| HLS live segments | Native TsMuxer validates in-memory |
| HLS upload egress | YouTube-style `file=` and path-style signed-query HTTP PUT delivery plus destination restart recovery are covered by unit tests and the `mixed.live.srt.h264.a1.bf2` HLS PUT probe |
| Recording | Readable file with correct streams/timestamps |
| Audio remap/downmix | Channel-level filtering is implemented for the default runtime; full audio-content matrix remains required |
| Custom encoding | Runtime output selection must stay rejected until custom args are applied by a transcoder backend |
| Bonded SRT ingest | Separate-process broadcast + backup tests |

## Container runtime smoke

`scripts/check/container-smoke.sh` proves the shipped runtime image, not just
process startup. It (1) checks the non-root user and no-mount health contract,
(2) proves real SRT egress under the shipped seccomp profile
(`distribution/docker/restream-seccomp.json`), and (3) drives a real H.264 RTMP
publisher through the image to RTMPS media egress using the mixed live harness.
The RTMPS probe mounts the checked-in trust certificate read-only at the same
absolute path used by the host-side harness; health remains a no-mount check.
The mixed mode verifies sink media, not only a successful socket connection.
The smoke also records what the engine's DEFAULT seccomp profile does as a
negative control (Docker 25+ denies the required `io_uring` syscalls; the
expected denial never fails the run, and a future default that allows them is
recorded as such). The live proofs need the harness
(`cargo xtask build-bench`); `--diagnostic-unconfined` adds a
`seccomp=unconfined` troubleshooting control that is never a deployment
recommendation. `tests/seccomp_profile.rs` statically pins the profile to "Moby
baseline + exactly the `io_uring` delta".

## SRT egress qualification

`scripts/harness/srt_final_qual.py` drives the SRT Compio Owner final
qualification (fresh-process fanout points sampled at 1 Hz with the Owner
metrics exposed in `/metrics/system` `egressShards`, concurrent connect bursts,
the `srt.slow-peer` mode, and the frozen-destination case) and derives the gates
and rates; `scripts/harness/test_srt_final_qual.py` tests that machinery. The
`srt.slow-peer` harness mode pauses APPLICATION delivery on one `RawSrtSink`
receiver (protocol timers and ACK/NAK keep running, unlike a SIGSTOPped peer) and
requires healthy siblings to keep progressing.

## Why two test tiers

Status: accepted and implemented.

This record explains why Restream uses two correctness tiers. It is not the
command reference; use [Testing](testing.md) to choose and run a gate.

### Decision

Restream has two correctness tiers:

1. **Unit and component tests** run through `cargo test`. They exercise pure
   logic, deterministic state machines, crafted packets/bytes, and bounded
   concurrency models without requiring a running service.
2. **Live tests** start the real `restream` binary, control it through the HTTP
   API, and publish/read media over real localhost RTMP, SRT, HTTP, and file
   boundaries.

Benchmarks are a separate measurement workflow, not a third correctness tier.

### Why there is no middle tier

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

### Tier responsibilities

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

### Correctness versus measurement

Correctness asks whether a protocol, timestamp, stream selection, lifecycle,
or recovery contract holds. Measurement asks how much CPU, memory, latency, or
throughput a known-correct path consumes.

Keep those workflows separate:

- correctness can use normal test profiles and parallelism when isolation is
  sound;
- measurement uses bench-profile binaries, fixed fixtures, and serial runs;
- a faster result never compensates for a weakened correctness oracle;
- a passing correctness test is not evidence of production-scale capacity.

### Implemented outcome

This testing-tier decision is implemented:

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

### Ongoing rules

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

## Stage boundary proof map

This map tracks the proof wall around stage boundaries. The goal is not line
coverage; it is to prove that packets, lifecycle state, capacity waits,
cancellation, and diagnostics cross each boundary without losing causality.

### Boundary Matrix

| Boundary | Contract to prove | Current proof | Next confidence target |
|---|---|---|---|
| Planner -> stage runtime | Planned `StageKey` and backend policy select the runtime that is registered, rendered in graph/status, and used by outputs. | Graph planner unit tests, backend-policy unit tests, engine terminal-stage tests, HLS/recording planned-key tests, and a property test over generated output mixes proving terminal-stage presence, edge input presence, unique stage keys, and no stale unqualified HEVC video stages. | Add new generated cases when new output protocols or stage kinds are introduced. |
| Runtime admission -> registry | `ensure_stage` creates exactly one live runtime, reuses live runtimes, replaces cancelled runtimes, and snapshots lifecycle/metrics. | Stage runtime unit tests plus mandatory-gate loom models for transcoder and TS muxer replacement races, including cancelled-stage replacement, concurrent creators, cleanup races, reader registration races, and codec metadata preservation. | No additional generic loom target is currently justified; it would duplicate the production locking rule already modeled by the stage-family replacement suites. |
| Control runtime -> media executor | Continuous container work progresses independently of control scheduling; dropping a control owner cancels but does not abort media finalization; old HLS cleanup/errors cannot remove or fail a replacement generation; normal file EOF permits another pass. | Executor cancellation/completion tests, blocked-control production tests for shared TS mux/HLS/file demux, HLS owner-abort final-segment and detached-replacement tests, recording owner-abort/writer-failure tests, and file coordinator cancellation/abort child-reaping plus removed/replacement-session cleanup tests in the shared concurrency gate. | No new synchronization primitive: existing ring/queue/stage loom models still cover waits and registry replacement; add a model only if those primitives change. |
| Source ring -> stage input (refill on the FFmpeg or stdin-writer thread) | Stage input starts at the correct keyframe/preroll point, emits TS bytes only for selected media, records first input once, refreshes parameter sets without awaiting engine state, exits on EOS/cancel, reports refilled bytes as queue depth, and an external child is reaped within the cancel grace even when its stdin writer is blocked. | Stage input codec-hint unit test, finite source-stage tests, source-stage chunking proptest, ring migration proptests/loom, filtered-packet first-input suppression, filtered-packet plus video EOS completion tests, `queue_refill_pulls_the_ring_on_the_reading_thread_until_eos`, `queue_refill_stops_on_cancel_while_waiting`, `refilled_batch_counts_toward_queue_depth`, `cancelled_reap_releases_a_writer_blocked_on_a_stalled_child`, and the `try_video_sequence_header` check in the file-ingest header test. | Add reconnect parameter-set refresh scenarios if a future reconnect bug appears. |
| Stage input -> backend | External and internal FFmpeg receive the same `FfmpegStagePlan` and startup policy; capacity waits are lifecycle-visible and cancellation-aware. | `build_ffmpeg_stage_plan` unit tests (`stage_runtime.rs`) proving one plan is constructed per `StageKind` and carries startup policy for both backends; `tests/transcoder.rs` integration coverage proving the external path (`build_stage_ffmpeg_args`) and internal path (`run_ffmpeg_transcode_with_scale`) each produce correct output from that plan; external capacity unit/harness evidence. | Table-test each `StageKind` into `FfmpegStagePlan` plus backend output equivalence for internal/external paths. |
| Backend -> output normalizer | Every backend emits through the normalizer; output timestamps are stage-local, non-negative, per-stream monotone, parameter sets are cached, first output is recorded once, and metrics match emitted packets. | Stage timeline unit tests, normalizer unit tests for first output, keyframe inference, split HEVC parameter sets, and a proptest over arbitrary interleaved audio/video packets asserting ring-visible timestamp/metric invariants. | Extend the property to generated split parameter-set/keyframe combinations if a future bug appears there. |
| Audio router boundary | Selected tracks, remap/downmix operations, prebuffer replay, EOS, and lifecycle cleanup preserve packet order and selected-track intent. | Audio-router unit tests for selected tracks, prebuffer replay, multi-track routing, stage sharing, and a property test proving selected-track metadata and packet reindexing stay in lockstep over generated interleaved audio/video packets. | Add a property for remap/downmix only if those operations become router-owned instead of FFmpeg-owned. |
| HLS segmenter boundary | Segmenter uses the planned protocol stage key, does not publish segments before init, exposes keyframe/no-segment states, and cleans runtime ownership. | HLS planned-key tests, fMP4 proptests, HLS publish loom, uploader terminal-stage tests. | Unit-test lifecycle/alert mapping for keyframe wait and no-segment states from the same snapshot. |
| Recording writer boundary | Recording metadata identity is persisted before failures, lifecycle is stage-owned, writer cleanup is visible, and media-library reads never rely on filename tokens. | Recording metadata tests, mixed harness recording identity proof, recording stage runtime ownership tests, and `recording_media_writer_failure_reports_failed_without_finalization` proving metadata survives writer failure. | Extend service-level failure coverage only when writer/finalization semantics change. |
| Runtime snapshot -> status/graph/alerts | Non-producing stage phases surface `blockedBy`, backend/capacity details, graph lifecycle details, diagnostics context, and alerts consistently. | Engine status tests, graph/status API tests, Phase 12 alert unit tests, and a table-driven stage phase contract that compares status JSON, graph node details, and alert classification for every non-producing `StagePhase`. | Add endpoint-level regression only if the serializer boundary changes. |
| Cancel/teardown -> observable cleanup | Cancellation wakes waiters, stops stages, removes runtime registry entries, and leaves operator-visible status causal rather than unknown. | AVIO/TS ring/ring migration loom, lifecycle guard tests, fault harness evidence, and the shared `cargo xtask concurrency fast` gate that keeps those loom models plus status/recovery contracts mandatory. | No uncovered wake/cancel interleaving remains in the current boundary map; add loom only with a new production primitive or stage-family registry shape. |

### Priority Order

All current priority targets are complete. New proof work should start by
adding a row to this map for the new stage family, protocol boundary, or
runtime primitive, then choose the lowest proof layer that catches the bug.

## Frontend boundary proof map

This map is the frontend counterpart to
[stage boundary proof map](#stage-boundary-proof-map). The goal is not
line coverage; it is to prove that operator-visible UI contracts — stream
reconnects, mutation convergence, session/auth redirects, route ownership,
and accessible structure — hold across the boundaries where the dashboard
talks to the backend, to real browser APIs, and to the operator.

### Boundary Matrix

| Boundary | Contract to prove | Current proof | Next confidence target |
|---|---|---|---|
| API client -> backend routes | Every dashboard read/mutation hits a canonical `/api/v1` route and method; multipart upload shape stays stable. | `test/frontend/frontend-api-contract.test.mjs`, plus the cross-stack `scripts/check/api-drift.mjs` gate run by `cargo xtask api-contract`. | None open; this is the most mature boundary — the only one with a hard-failing CI gate rather than an advisory test run. |
| SSE/log-stream reconnect | One connection per filter/scope, paused while the tab is hidden, resumed from the last event id on visibility, replaced (not duplicated) when scope changes, and a superseded source's events never reach the caller. | `frontend-log-stream.test.mjs`, `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`, `frontend-overview-activity-stream.test.mjs` for scripted scenarios; `frontend-log-stream-interleaving.property.test.mjs` model-checks the staleness guard (`source !== openedSource`) against randomized `sync()`/`emit()` interleavings, the frontend analog of a loom test. | The interleaving proof covers `core/log-stream.ts` only; `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`, and `frontend-overview-activity-stream.test.mjs` still rely on scripted scenarios for their own reconnect guards. |
| Dashboard runtime polling and mutation convergence | Output/pipeline start, stop, edit, and delete mutations optimistically update the UI and converge with the shared runtime poller; nothing starts a second poller of its own. | `test/frontend/dashboard-contract/output-mutations.test.mjs`, `pipeline-mutations.test.mjs`, `runtime-modes.test.mjs`, `runtime-polling.test.mjs`, `frontend-publisher-health-contract.test.mjs`. | None open. |
| Auth/session boundary | Unauthenticated requests redirect to `/login`; a successful login reaches the dashboard and preserves the intended destination. | `test/frontend/frontend-browser-dom.spec.ts` (login flow, audio-track picker, HLS retry, mobile overflow). | None open. |
| HLS playback and fatal-error retry | The managed HLS controller waits for manifest readiness, destroys and recreates the player on a fatal error, supports alternate-audio-track switching, and clears state on stage teardown. | `test/frontend/hls-player.spec.ts` (real browser playback), `npm run test:frontend:browser-dom` (audio-track picker), backend `cargo test hls_fmp4` and `cargo bench --bench hls_fmp4_cost` for the segment/publication side. | None open; see `docs/testing.md`'s fMP4 preview section for the full ladder. |
| Dashboard route ownership and navigation history | Each mode route (overview, pipeline, media, settings, status, incidents, telemetry) is rendered by the v2 owner with no leftover v1 fallback; browser back/forward is one predictable history step per navigation; primary tab focus survives background refresh. | `test/frontend/redesign/seed-navigation.spec.ts`, `seed-media-route.spec.ts`, `seed-settings-route.spec.ts`, `seed-status-route.spec.ts`, `seed-surfaces.spec.ts`, `frontend-ops-navigation.test.mjs`, `frontend-build-smoke.test.mjs`. | None open. |
| Accessible structure | Heading outline stays in true reading order and operator-clean across default routes, interactive controls expose accessible names, no serious/critical axe violations, and keyboard focus reaches primary actions. | `test/frontend/redesign/visual-accessibility.spec.ts`. | The strict route-heading-order assertion only covers Overview, Operate, Inspect, and Monitor; extend to Media/Settings/Status/Incidents/Telemetry if a heading-order regression ever surfaces there. |
| Scale and large-fleet rendering | The dashboard stays responsive and collapses repeated entries (egress leaves, non-egress branch stages) once pipeline/output counts get large. | `test/frontend/redesign/seed-scale.spec.ts`, `frontend-pipeline-workspace.test.mjs` (processing-graph collapse cases). | None open. |

### Current Mandatory Surfaces

These frontend tests must never regress silently. Touching the boundary they
cover requires an equal or stronger replacement proof in the same change,
mirroring [`concurrency-proofing.md`](concurrency-proofing.md)'s backend
list:

- `test/frontend/frontend-api-contract.test.mjs`
- `test/frontend/dashboard-contract/output-mutations.test.mjs`,
  `pipeline-mutations.test.mjs`, `runtime-modes.test.mjs`,
  `runtime-polling.test.mjs`
- `test/frontend/frontend-publisher-health-contract.test.mjs`
- `test/frontend/frontend-log-stream.test.mjs`,
  `frontend-log-stream-interleaving.property.test.mjs`,
  `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`
- `test/frontend/hls-player.spec.ts`
- `test/frontend/frontend-browser-dom.spec.ts`
- `test/frontend/redesign/seed-navigation.spec.ts`,
  `seed-media-route.spec.ts`, `seed-settings-route.spec.ts`,
  `seed-status-route.spec.ts`
- `test/frontend/redesign/visual-accessibility.spec.ts`
- `test/frontend/frontend-build-smoke.test.mjs`

### Priority Order

All current priority targets are complete. New proof work should start by
adding a row to this map for the new UI surface, stream contract, or route
boundary, then choose the lowest layer that can catch the bug (TypeScript
unit -> fake-DOM scenario matrix -> browser-native Playwright -> full
`test:e2e`), per `docs/testing.md`'s layered UI strategy.

## Regression artifacts

This index preserves the historical failure evidence that drove the first
architecture phases. Generated run directories stay under `.local/artifacts/` and
are not committed; the durable guardrail is the checked-in fixture, harness
mode, or proof gate listed here.

### Historical failure classes

| Historical failure class | Preserved evidence / replay path | Guardrail |
|---|---|---|
| External H.265 capacity or zero-output stall | HEVC checked-in fixtures: `test/fixtures/transport/correctness-h265.ts`, `test/fixtures/transport/bench-h265-1_5m.ts`, `test/fixtures/transport/bench-h265-1_5m-2a.ts`, plus mixed HEVC modes such as `mixed.live.srt.h265.a1.bf2` and `mixed.live.srt.h265.a2.bf2`. | Dependency-aware health and alert tests cover `waitingForCapacity`; `cargo xtask concurrency fast` includes external stage liveness checks. |
| Low-CPU external-capacity collapse | Resource sweep artifacts are generated under `.local/artifacts/resource-sweep/`; authoritative CSV baselines are documented in `docs/resource-sweep.md`. | `target/bench/test_harness resource-sweep` and `docs/matrix-resource-constraints.md` preserve the capacity/RSS contract. |
| Internal-transcoder timestamp discontinuity | `tests/transcoder.rs` and `tests/av_sync.rs` use checked-in MPEG-TS fixtures through `src/test_fixtures.rs`. | `cargo xtask concurrency fast` runs chunked internal-transcoder timestamp tests and source-stage proptests. |
| Recording `.tmp.mp4` or wrong-case media selection | Recording metadata tests in `tests/api.rs` and mixed harness playback tests reject temporary outputs and metadata-less filename fallback. | `cargo test media_recording_identity --bin test_harness` and API media-library metadata tests preserve recording identity by `pipelineId`/`recordingId`. |

### Adding evidence

When adding a new historical failure artifact, prefer one of these durable
forms:

- a checked-in media fixture registered in `src/test_fixtures.rs`;
- a focused unit/integration test that recreates the failure from an existing
  fixture;
- a harness mode that writes `manifest.json` and `results.jsonl` under
  `.local/artifacts/<run-id>/`;
- a documented benchmark or sweep baseline with its replay command.

Do not commit ad-hoc generated run directories. If a generated artifact is
needed for triage, store it under `.local/artifacts/<run-id>/` and reference the
run id from the issue, PR or commit.
