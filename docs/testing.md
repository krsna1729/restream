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
through `media::rtmp::fuzz_entry` and egress-private ones through
`media::egress::fuzz_entry`, which exist only under `--cfg fuzzing`.

| Target | Input | Production code |
|---|---|---|
| `flv_video_tag` | RTMP ingest video tag | `classify_flv_video_packet`, `flv_avcc_config_annexb_parameter_sets`, `parse_flv_video_meta` (SPS) |
| `flv_audio_tag` | RTMP ingest audio tag | `parse_flv_audio_meta` (AudioSpecificConfig) |
| `avcc_annexb` | H.264 from either ingest | `parse_avcc_config`, `avcc_to_annexb`, `annexb_to_avcc`, `build_avcc_sequence_header` (must read back) |
| `hevc_enhanced_rtmp` | HEVC Annex-B from SRT ingest | `build_hevc_enhanced_rtmp_sequence_header` (SPS profile/tier/level), coded-frame packer |
| `rtmp_server_responses` | destination server bytes after the handshake | `RtmpSessionCore::handle_server_input` |
| `rtmp_client_requests` | publisher bytes after the handshake | ingest `ServerSession::handle_input` with the message-size limit |
| `ts_demux` | SRT ingest MPEG-TS, sync-forced or raw | `TsDemuxer` (PAT/PMT, PES, `mpegts_probe`) |
| `http_response` | HLS PUT destination server response, split into reads | `http1::ResponseReader` (the response must not depend on where reads split it) |

```sh
cd fuzz
./seed-corpus.sh   # seeds ts_demux and http_response
cargo +nightly fuzz run <target> -- -max_total_time=60
```

CI (`Parser fuzz smoke`) runs every target for 30 s. A crash becomes a
regression unit test next to the parser before the fix lands.

### Miri and AddressSanitizer

Two narrow memory-safety jobs complement the fuzz targets. Neither copies a
whole-suite matrix: Miri cannot execute FFmpeg, io_uring or sockets, and the
linked FFmpeg is not instrumented under ASan.

| CI job | Modules | Why these |
|---|---|---|
| `Miri (pure modules Miri can execute)` | the kTLS control-message parser, `egress::leaf_arena`, `egress::scheduler`, `codec::bits` | the kTLS parser is project `unsafe` (raw cmsg reads); the others are what Miri can run quickly, so it checks their std and dependency use |
| `AddressSanitizer (FFI and syscall modules)` | `avio` (blocking queue), `egress::tls`, `compio_tcp`, `external_transcoder`, `timing`, `rtmp::listener`, `rtmp::tests` (live listener and socket buffers), `tcp_stats` (`getsockopt` into fixed buffers), `runtime_info` (FFmpeg C strings) | Rust-side use of FFmpeg, kTLS and socket memory |

Both jobs first run `scripts/ci/require-test-filters.sh`, which fails when any
filter selects no test: a renamed module cannot silently drop out.

```sh
MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=8 \
  cargo +nightly miri test --lib -- media::egress::tls::ktls::tests::ancillary media::egress::leaf_arena
RUSTFLAGS=-Zsanitizer=address ASAN_OPTIONS=detect_leaks=0 \
  cargo +nightly test -Zbuild-std --target x86_64-unknown-linux-gnu --lib -- media::avio
```

`ring_buffer` and `egress::journal` are left out of Miri (8 and over 25
minutes there; neither has project `unsafe`). A module gains `unsafe` or an
FFI call: add it to the matching job.

### Kani proofs

Rung 4 of the [assurance roadmap](assurance-roadmap.md): bounded model checks
of production functions, run by the `Kani (bounded proofs of egress
primitives)` CI job (`cargo kani --lib -Z stubbing`, Kani 0.68). Each proof
sits in a `#[cfg(kani)] mod kani_proofs` beside its code.

| Primitive | Proof | Property |
|---|---|---|
| `egress::leaf_arena` | `a_key_resolves_exactly_while_its_leaf_is_live` | any 4 inserts, reserves, fills and removals with fresh and stale keys: a key resolves exactly while its leaf is live; `len` equals the live count and never exceeds capacity |
| `egress::scheduler::ReadyQueue` | `pushes_and_pops_are_fifo_once_per_key_at_capacity_{one,two}` | any order of pushes and pops over a fixed sequence of epoch-0 keys (slots 0, 1, 0 at capacity 1; 0, 1, 2 at capacity 2): FIFO, each key queued at most once, `AlreadyQueued` and `Full` exactly when due |
| `egress::scheduler::ReadyQueue` | `removing_the_{older,newer}_leaf_never_leaves_it_queued` | two concrete traces: a removed leaf's key is never a member again; the other keeps its place; the slot's next leaf queues behind it |
| `egress::policy::WorkBudget` | `a_visit_never_takes_more_than_its_budget`, `debits_saturate` | a visit that asks only while not exhausted, at most `remaining_bytes()` per unit, never exceeds its unit or byte limit, for any limits; debits saturate (`Instant::now` is stubbed) |

Bounds are small (a few steps over two or three slots) because CBMC must also
model the std containers. Symbolic keys that reach `VecDeque` copies and
`retain` exhaust memory, so the `ReadyQueue` proofs make the operation choice
symbolic and keep the keys concrete; `pop`'s skip of a stale entry is covered
by unit tests, not by these proofs. `kani::cover!` statements show each proof
reaches its interesting branches (a full queue, a key already queued, a stale
arena key, a slot reused at a new epoch, a visit ended by units and by bytes);
the CI job fails if any is unsatisfiable. Kani builds abort on panic by design, so
the `panic = "abort"` build guard in `src/lib.rs` skips `cfg(kani)`. A change
that breaks one of these properties fails its proof; a new primitive proof
raises `KANI_PROOFS` in the job.

```sh
cargo install --locked kani-verifier --version 0.68.0 && cargo kani setup
cargo kani --lib -Z stubbing --harness a_key_resolves_exactly_while_its_leaf_is_live
```

## Frontend test split

Frontend confidence is intentionally split between TypeScript ownership and
compiled-bundle smoke coverage. Current invariant coverage by UI contract
boundary — runtime state and log stream reconnects, mutation convergence,
auth/session and media playback — is tracked in
[frontend boundary proof map](#frontend-boundary-proof-map), the
frontend counterpart to [stage boundary proof map](#stage-boundary-proof-map):

- `npm run test:frontend` runs the Node-based frontend suites from a temporary
  sourcemapped build of `web/ts/**`, then finishes with a smaller smoke pass
  against the shipped `public/js/**` bundle.
- `npm run test:frontend:coverage` keeps the same split, but reports coverage
  back onto the deterministic TypeScript modules that the Node/fake-DOM suite
  is meant to own. This is the main frontend coverage report, run on every PR
  by the `Frontend unit` CI job — advisory, not threshold-gated, matching the
  backend `coverage` job's posture (`cargo llvm-cov` also reports every PR
  without failing the build on a percentage). Neither stack fails CI on a
  coverage number; both make the trend visible.
- `npm run test:frontend:js-smoke` is the minimal direct guard for generated
  `public/js/**`; use it when you only need to verify the compiled artifact.

This keeps detailed behavior and coverage attached to the TypeScript source of
truth without dropping confidence in the emitted browser bundle, while avoiding
misleading Node-only coverage targets for browser-heavy modules.

### Layered UI strategy

Treat frontend confidence as three layers, each owning a different kind of risk:

| Layer | Purpose | Typical command |
|---|---|---|
| TypeScript/source logic | Keep parsing, helpers, API choke points, stream clients and workspace models deterministic and cheap. | `npm run test:frontend` |
| Full app/browser integration | Prove login, navigation, media playback, real network wiring, and end-to-end runtime behavior against an isolated app with committed fixtures. | `npm run test:e2e` |
| Backend media path | Prove the preview segments the browser plays. | `cargo test hls_fmp4`, `cargo bench --profile bench --bench hls_fmp4_cost` |

`npm run test:e2e` is self-contained: it builds the native-linked debug app and
frontend, seeds the required checked-in multi-audio fixture into an isolated
`.local/e2e/` media directory, waits for `/healthz`, runs Playwright, and
stops only the app process it started. Do not manually start a dashboard before
using it. Each Playwright case makes the pipelines it needs from that fixture.

Use the lowest layer that can actually catch the bug. Move upward only when the
lower layer cannot prove the behavior.

### Property and interleaving tests

Hand-picked scenario fixtures prove the cases someone thought to write down;
they miss the cases nobody thought of. For pure state-derivation logic and
for closures that guard against stale-callback races (the frontend analog of
a wait/cancel/reconnect boundary), add a [fast-check](https://github.com/dubzzz/fast-check)
property test instead of another hand-picked case:

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
- HLS push or pull ingest, or executor isolation — `hls-push-duty`, whose
  baseline arm (`--hls-publishers 0`) and treatment arms (`--hls-publishers N`,
  `--pull-sessions N`) differ only by the HLS addition;
- broad regression confidence — a catalog suite after the scoped mode passes;
- performance or capacity — a measurement workflow, run serially with
  bench-profile binaries.

Use the catalog's current inspection commands to resolve a mode and review its
services, scenarios, checks, timeouts, and artifacts before spending time on a
live run.

### HLS push ingest isolation (`hls-push-duty`)

`hls-push-duty` is the harness gate for HLS push ingest and the treatment arm
of an executor-isolation measurement: does adding HLS ingest sessions degrade
the existing RTMP/SRT ingest-and-egress guarantees? It starts one bench-profile
Restream, then builds two arms that differ only by the HLS addition:

- **Baseline arm** (`--base-publishers N`, default 2): `N` RTMP publishers,
  each an ingest input and one RTMP sink output.
- **Treatment arm** (`--hls-publishers N`, default 0): the same base load plus
  `N` pipelines whose input is an HLS push session, driven by one FFmpeg
  process each (`-re -f hls -method PUT`, or POST with `--method post`), to
  the same RTMP sink output kind.
- **Pull arm** (`--pull-sessions N`, default 0): the same base load plus one
  `pull-source` RTMP pipeline and `N` pipelines whose selected input is an
  `hlsPull` of the source's own preview master (the harness's session cookie
  as a request header), each to the same RTMP sink output kind. The source is
  a pipeline of its own so the base pipelines do not also package the pulled
  preview; pull pipelines are held to the `hls-pull-duty` media checks.

Every pipeline's output goes to its own in-process RTMP sink, so delivery is
asserted per pipeline (video, audio, a keyframe, monotonic DTS) and the egress
side of the fabric carries real work. `--hls-publishers 0` is the baseline and
`--hls-publishers 4` (or `--pull-sessions 4`) the treatment, with the same
base load in both.

```sh
scripts/harness/run.sh hls-push-duty
scripts/harness/run.sh hls-push-duty -- --hls-publishers 4 --segment-type fmp4
scripts/harness/run.sh hls-push-duty -- --hls-publishers 2 --method post
scripts/harness/run.sh hls-push-duty -- --pull-sessions 4 --duration-secs 60
scripts/harness/run.sh hls-push-duty -- --hls-publishers 8 --duration-secs 60 --poll-usage
```

| Option | Default | Meaning |
|---|---|---|
| `--base-publishers N` | `2` | RTMP baseline publishers with one sink output each |
| `--hls-publishers N` | `0` | Extra HLS push pipelines with one sink output each |
| `--pull-sessions N` | `0` | HLS pull pipelines (plus one pull-source pipeline) with one sink output each |
| `--segment-type ts\|fmp4` | `ts` | Container the HLS arm pushes |
| `--method put\|post` | `put` | HTTP method the HLS arm uploads with |
| `--duration-secs N` | `12` | Rated window after every input has published |
| `--segment-secs N` | `2` | `-hls_time` for the HLS arm |
| `--poll-usage` | off | Start Restream with `RESTREAM_MEDIA_POLL_USAGE=1` (per-poll user/system time and faults) |

The result JSON (`.local/artifacts/hls-push-duty/report.json`) carries
`pipelines[].publishedPackets` (per-pipeline video/audio/keyframe counts from
its sink), `pipelines[].input` (bytes received and GOP keyframe count),
`pipelines[].egress` (resync count, `stalled`, queue overflows and RX/TX drop
counters), `pipelines[].executor` (the pipeline's media executor services over
the rated window: `polls`, `cpuUs`, `cpuUsPerPoll`, `busyUs`, the largest
lifetime `maxCpuUs`, and `pollsOverCpu4Ms`), the raw `mediaExecutor` snapshot
(`queueLatencyMaxUs` and the per-class rows of
[observability](observability.md#media-executor-mediaexecutor)), the raw
`egressShards` list, `wallClockSecs`, `windowSecs`, and
`environment.cpuCount` so runs are comparable.

`pollCpu` is the per-class executor window: every counter subtracted from its
value as the window opened, `cpuUsPerPoll`, the lifetime `maxCpuUs` and
`maxPollUs`, `perMinutePerSession` (polls above 1, 4 and 16 ms of thread CPU
per window minute per active service of the class — for `hlsIngest`, per HLS
session), and with `--poll-usage` the window's `usage` sums. `hostMemory`
separates the allocator from the kernel around the same window: `/proc/vmstat`
deltas of `compact_stall`, `allocstall_*`, `thp_fault_alloc` and
`pgmajfault`, the Restream cgroup's `memoryEvents` deltas, and the host's THP
`enabled`/`defrag` modes. FFmpeg is resolved through `src/ffmpeg_binary.rs`
and the source through `src/test_fixtures.rs`.

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
requires healthy siblings to keep progressing; with `SLOW_PEER_EXACT_OWNER=1` it
also requires one live SRT shard, so every output shares one IPv4 Owner. The
`srt.admission` mode caps each pool at `ADMISSION_SHARDS` (default 1) egress
shards and fans one 8 Mbit/s feed out to `ADMISSION_OUTPUTS` (default 96 per
shard) SRT outputs, past the pool's admission ceiling: refused outputs must
report `capacityRefused` and send nothing, every admitted output must keep
progressing, and no `egress-shard-<n>` thread may reach 62% busy.

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
| Planner -> stage runtime | Planned `StageKey` and `select_backend` choose the runtime that is registered, rendered in graph/status, and used by outputs. | Graph planner unit tests, `select_backend` unit tests, engine terminal-stage tests, HLS/recording planned-key tests, and a property test over generated output mixes proving terminal-stage presence, edge input presence, unique stage keys, and no stale unqualified HEVC video stages. | Add new generated cases when new output protocols or stage kinds are introduced. |
| Runtime admission -> registry | `ensure_stage` creates exactly one live runtime, reuses live runtimes, replaces cancelled runtimes, and snapshots lifecycle/metrics. | Stage runtime unit tests plus mandatory-gate loom models for transcoder and TS muxer replacement races, including cancelled-stage replacement, concurrent creators, cleanup races, reader registration races, and codec metadata preservation. | No additional generic loom target is currently justified; it would duplicate the production locking rule already modeled by the stage-family replacement suites. |
| Control runtime -> media executor | Continuous container work progresses independently of control scheduling; dropping a control owner cancels but does not abort media finalization; old HLS cleanup/errors cannot remove or fail a replacement generation; normal file EOF permits another pass. | Executor cancellation/completion tests, blocked-control production tests for shared TS mux/HLS/file demux, HLS owner-abort final-segment and detached-replacement tests, recording owner-abort/writer-failure tests, and file coordinator cancellation/abort child-reaping plus removed/replacement-session cleanup tests in the shared concurrency gate. | No new synchronization primitive: existing ring/queue/stage loom models still cover waits and registry replacement; add a model only if those primitives change. |
| Source ring -> stage input (refill on the stdin-writer thread) | Stage input starts at the correct keyframe/preroll point, emits TS bytes only for selected media, records first input once, refreshes parameter sets without awaiting engine state, exits on EOS/cancel, and an external child is reaped within the cancel grace even when its stdin writer is blocked. | Stage input codec-hint unit test, finite source-stage tests, source-stage chunking proptest, ring migration proptests/loom, filtered-packet first-input suppression, filtered-packet plus video EOS completion tests, `refill_pulls_the_ring_on_the_reading_thread_until_eos`, `refill_stops_on_cancel_while_waiting`, `cancelled_reap_releases_a_writer_blocked_on_a_stalled_child`, and the `try_video_sequence_header` check in the file-ingest header test. | Add reconnect parameter-set refresh scenarios if a future reconnect bug appears. |
| Stage input -> backend | External FFmpeg receives the `FfmpegStagePlan` and startup policy; capacity waits are lifecycle-visible and cancellation-aware. | `build_ffmpeg_stage_plan` unit tests (`stage_runtime.rs`) proving one plan is constructed per `StageKind` and carries startup policy; `tests/transcoder.rs` integration coverage proving the external path (`build_stage_ffmpeg_args`) produces correct output from that plan; external capacity unit/harness evidence. | Table-test each `StageKind` into `FfmpegStagePlan`. |
| Backend -> output normalizer | The external transcoder's stdout reader emits every packet through the normalizer; output timestamps are stage-local, non-negative, per-stream monotone, parameter sets are cached, first output is recorded once, and metrics match emitted packets. | Stage timeline unit tests, normalizer unit tests for first output, keyframe inference, split HEVC parameter sets, and a proptest over arbitrary interleaved audio/video packets asserting ring-visible timestamp/metric invariants. | Extend the property to generated split parameter-set/keyframe combinations if a future bug appears there. |
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
reconnects, mutation convergence, session/auth redirects and media
playback — hold across the boundaries where the dashboard
talks to the backend, to real browser APIs, and to the operator.

### Boundary Matrix

| Boundary | Contract to prove | Current proof | Next confidence target |
|---|---|---|---|
| API client -> backend routes | Every dashboard read/mutation hits a canonical `/api/v1` route and method; multipart upload shape stays stable. | `test/frontend/frontend-api-contract.test.mjs`, plus the cross-stack `scripts/check/api-drift.mjs` gate run by `cargo xtask api-contract`. | None open; the only boundary with a hard-failing CI gate rather than an advisory test run. |
| Runtime state stream | One snapshot, then deltas in revision order; a gap reconnects from the last revision; a silent stream is reported, not shown as live. | `test/frontend/frontend-runtime-state-stream.test.mjs`. | None open. |
| SSE/log-stream reconnect | One connection per filter, resumed from the last event id, replaced (not duplicated) when the filter changes, and a superseded source's events never reach the caller. | `frontend-log-stream.test.mjs`; `frontend-log-stream-interleaving.property.test.mjs` model-checks the staleness guard (`source !== openedSource`) against randomized `sync()`/`emit()` interleavings. | None open. |
| Workspace models and mutations | Each control-room workspace derives its view from the store and sends mutations through `core/api.ts`. | `test/frontend/control-room-*.test.mjs`. | None open. |
| Auth, navigation and media against a running engine | Login, `?w=` workspaces, mutations converging in runtime state, HLS program and input previews (audio-track choice, stall and pause states), logs and diagnostics. | `test/frontend/control-room.spec.ts` (`npm run test:e2e`). | None open. |

### Current Mandatory Surfaces

These frontend tests must never regress silently. Touching the boundary they
cover requires an equal or stronger replacement proof in the same change,
mirroring [`concurrency-proofing.md`](concurrency-proofing.md)'s backend
list:

- `test/frontend/frontend-api-contract.test.mjs`
- `test/frontend/frontend-runtime-state-stream.test.mjs`
- `test/frontend/frontend-log-stream.test.mjs`,
  `frontend-log-stream-interleaving.property.test.mjs`
- `test/frontend/control-room.spec.ts`

### Priority Order

All current priority targets are complete. New proof work should start by
adding a row to this map for the new UI surface, stream contract, or route
boundary, then choose the lowest layer that can catch the bug (TypeScript
unit -> `test:e2e`), per the layered UI strategy above.

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
