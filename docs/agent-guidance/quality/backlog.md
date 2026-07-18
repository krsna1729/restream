# Quality Backlog

Prioritized work queue for the autonomous quality loop. Format and grooming
rules: `docs/agent-guidance/skills/backlog-groom/SKILL.md`. Execution protocol:
`docs/agent-guidance/skills/quality-loop/SKILL.md`. Top of file = highest
priority.

Dimensions: `proof` (correct/proven) · `resilience` (reliable/resilient) ·
`modularity` · `efficiency` · `performance` · `groom`.
Tiers: `haiku` (read-only audit) · `sonnet` (scoped code+test) · `opus`
(concurrency/hot-path architecture).

## Contents

- [Open](#open)
- [Blocked](#blocked)
- [Archive](#archive)

## Open

### Q-001 [proof] [sonnet] Establish the per-module coverage map
- Goal: a per-module line/branch coverage table for `src/` recorded in the
  journal, with follow-up `[proof]` items filed for the 3 weakest
  `src/media/` modules.
- Files: none modified (measurement only); output lands in
  `docs/agent-guidance/quality/journal.md` + new backlog items.
- Gates: `scripts/build/resource-limit.sh cargo llvm-cov --summary-only` completes
  clean (kill-check media processes first; this is a heavy build).
- Context: cargo-llvm-cov is installed. The stale root `coverage.lcov` is from
  2026-06-24 and gitignored; a fresh map is the seed for invariant-coverage
  work per the proof-sweep skill.
- Status: done (Mapped at `5f1c10f4`; follow-ups Q-014 through Q-016 filed;
  2026-07-17 by codex; Filed: 2026-07-03 by bootstrap)

### Q-014 [proof] [sonnet] Bind or retire the zero-execution FFmpeg operation layer
- Goal: `ffmpeg/operation.rs` and `ffmpeg/operation_compiler.rs` either become
  part of an actual backend-owned execution path with a mutation-proven mapping
  test, or are removed as unused indirection. Do not add incidental tests for
  code that no production path consumes.
- Files: `src/media/ffmpeg/operation.rs`,
  `src/media/ffmpeg/operation_compiler.rs`, `src/media/ffmpeg/mod.rs`, and the
  owning backend/test if the layer is retained.
- Gates: focused backend/stage-runtime tests; `cargo fmt --all --check`;
  standard clippy and test gates.
- Context: Q-001 measured 0/6 and 0/60 covered lines respectively, and call-site
  search found no consumer of `compile_operation`. The module comments claim
  both backends consume this layer, so coverage-only tests would preserve a
  false architectural assumption instead of proving runtime behavior.
- Status: done (retired: `operation.rs`/`operation_compiler.rs` had zero
  consumers anywhere in `src/`, `test/`, `benches/`, and both FFmpeg
  backends actually consume `FfmpegStagePlan` directly per
  `ffmpeg/backend.rs`'s trait signatures — the module docs and
  `docs/stage-boundary-proof-map.md` claiming a shared "compiled operation"
  were architecturally false; deleted both files, removed their `pub mod`
  lines from `ffmpeg/mod.rs`, and corrected the proof-map row to cite the
  real `FfmpegStagePlan`-based proof in `stage_runtime.rs` and
  `tests/transcoder.rs`; 2026-07-18 by codex; Filed: 2026-07-17 by Q-001)

### Q-015 [proof] [sonnet] Prove adversarial SRT crypto option boundaries
- Goal: deterministic mutation-proven coverage for plaintext versus encrypted
  resolution, URL default key length, every supported key length, interior-NUL
  passphrases, and FFI option failures through the existing error surface.
- Files: `src/media/srt/crypto.rs`, `src/media/srt_tests.rs`.
- Gates: `scripts/build/resource-limit.sh cargo test srt_crypto --lib`;
  `cargo fmt --all --check`; standard clippy and test gates.
- Context: Q-001 measured 13/80 covered lines (16.25%) in the crypto adapter.
  Current higher-layer validation is strong, but the last conversion and FFI
  boundary remains mostly unexecuted.
- Status: done (2026-07-18 by codex). Coverage landed as planned, and it
  surfaced a real production bug: bonded SRT egress always failed to
  connect when a passphrase or non-empty StreamID was configured, because
  libsrt's per-member bonding config (`srt_config_add`) silently rejects
  `SRTO_PASSPHRASE`/`SRTO_PBKEYLEN`/`SRTO_STREAMID`. Fixed by applying
  those as group-wide socket options via `srt_setsockopt` before
  `srt_connect_group`, matching the already-correct non-bonded path. See
  journal for full root cause and the FFI regression tests that pin both
  the fix and the underlying libsrt limitation.

### Q-016 [proof] [sonnet] Prove RTMP session fault transitions
- Goal: the smallest deterministic component proof for malformed or truncated
  RTMP session input asserts both the surfaced protocol error and complete
  session/registration cleanup. Reuse the existing session harness and avoid a
  duplicate live pipeline.
- Files: `src/media/rtmp.rs`, `src/media/rtmp/tests.rs`.
- Gates: `scripts/build/resource-limit.sh cargo test rtmp --lib`;
  `cargo fmt --all --check`; standard clippy and test gates.
- Context: Q-001 measured 221/1,301 covered lines (16.99%) despite strong FLV,
  timestamp, and state-helper proofs. The remaining gap is concentrated in the
  assembled session path where malformed input must not leak registrations or
  crash the engine.
- Status: done (2026-07-18 by codex). Found a real registration-leak bug, not
  just a coverage gap: `handle_rtmp_client`'s main-loop `socket.read` arm
  `?`-early-returned on both a raw read error and a `session.handle_input`
  chunk-deserialization error, bypassing the post-loop ingest-cleanup block
  entirely, so a single malformed RTMP chunk byte sent after a publisher
  registered left that pipeline stuck in `engine.ingests.active` forever.
  Fixed by routing both faults through the same `break Some((phase, reason,
  true))` + post-loop-cleanup pattern the adjacent session-result-error arm
  already used. Two new proofs added to `src/media/rtmp/tests.rs` drive a
  real `handle_rtmp_client` over a loopback socket with a real `rml_rtmp`
  client publish handshake: one injects the deterministic single-byte chunk
  fault (`NoPreviousChunkOnStream`) after publish and asserts cleanup; one
  sends a truncated valid chunk header then disconnects and asserts the
  ordinary-EOF path still cleans up with no error. See journal for full root
  cause and the byte-level trigger mechanism. Filed: 2026-07-17 by Q-001)

### Q-017 [proof] [sonnet] Reject incomplete copied frontend dependency caches
- Goal: worktree hydration must not report `node_modules` ready when any
  dependency required by `npm run build:frontend` is absent; a synthetic stale
  cache regression must fail the readiness check and a complete cache must
  pass it.
- Files: `scripts/agent/worktree.sh` and a focused script-level regression
  check.
- Gates: focused worktree dependency regression; `bash -n
  scripts/agent/worktree.sh`; `npm run build:frontend`.
- Context: Q-001 initially failed four API static-asset tests because generated
  assets were absent. The repair then exposed a stale copied `node_modules`
  tree that the helper called ready despite missing React packages, causing the
  canonical frontend build to fail until `npm ci`.
- Status: done (Synthetic stale and corrupted caches rejected; current
  dependency tree and full frontend suite pass; 2026-07-17 by codex; Filed:
  2026-07-17 during Q-001)

### Q-018 [resilience] [sonnet] Reject malformed PMT sections before state mutation
- Goal: `TsDemuxer::parse_pmt` must validate the complete PMT section,
  program-info span, and every ES descriptor span before changing
  `pmt_version`, the stream map, or in-flight PES accumulators; a malformed
  newer PMT followed by a valid PMT with the same version must leave the old
  state intact and then accept the valid update.
- Files: `src/media/mpegts.rs`,
  `src/media/mpegts_tests/tables_and_sync.rs`.
- Gates: break-it-first focused regression; `scripts/build/resource-limit.sh
  cargo test media::mpegts::tests::tables_and_sync --lib`; relevant
  `high_performance_data_path` MPEG-TS demux benchmark before/after; `cargo fmt
  --all --check`; standard clippy and test gates.
- Context: Q-002 found that version and stream state are committed before
  `program_info_length` and `ES_info_length` are known to fit the section. An
  oversized declared length can clear a working stream map and make the later
  valid retransmission look like a duplicate.
- Status: done (Malformed program-info and ES-descriptor spans are rejected
  before version/stream mutation; valid same-version retransmissions recover;
  2026-07-17 by codex; Filed: 2026-07-17 by Q-002)

### Q-019 [resilience] [sonnet] Make MPEG-TS SPS probing fail closed on exhausted bits
- Goal: `mpegts_probe::parse_h264_sps` and
  `mpegts_probe::parse_h265_sps` must stop on truncated or overlong
  Exp-Golomb fields without panicking, wrapping dimensions, allocating from
  untrusted counts, or publishing partial metadata; deterministic crafted
  vectors must exercise the bit-exhaustion and crop-underflow boundaries.
- Files: `src/media/mpegts_probe.rs`, `src/media/mpegts_tests.rs`.
- Gates: break-it-first focused regressions; `scripts/build/resource-limit.sh
  cargo test media::mpegts::tests --lib`; relevant
  `high_performance_data_path` MPEG-TS demux benchmark before/after; `cargo fmt
  --all --check`; standard clippy and test gates.
- Context: Q-002 found only a two-byte H.265 smoke case. The MPEG-TS probe bit
  reader substitutes zero after exhaustion and permits a 32-zero
  Exp-Golomb prefix, leaving shift overflow, arithmetic underflow, and
  attacker-controlled loop-count assumptions unproved.
- Status: done (Fail-closed SPS parsing: partial-metadata commit removed, bit
  reader and Exp-Golomb decode return `Option` on exhaustion/overflow instead
  of substituting zero, and H.265 syntax counts are bounded; also fixed a
  latent H.264 scaling-list size-selection bug found in the same pass;
  2026-07-18 by codex; Filed: 2026-07-17 by Q-002)

### Q-020 [resilience] [sonnet] Prove AVCC declared-length rejection consistently
- Goal: `codec::parse_avcc_config`,
  `rtmp::flv::flv_avcc_config_annexb_parameter_sets`, and
  `hls::fmp4::parse_avcc_box` must reject truncated SPS/PPS count and length
  fields, including maximum declared lengths with tiny backing buffers,
  without returning partial parameter-set state; consolidate duplicate
  fixtures where the same AVCC record can serve all three owners.
- Files: `src/media/codec.rs`, `src/media/rtmp/tests.rs`,
  `src/media/hls/fmp4.rs`.
- Gates: break-it-first focused regressions; scoped codec/RTMP/HLS tests;
  `cargo fmt --all --check`; standard clippy and test gates.
- Context: Q-002 found basic truncation coverage in codec and RTMP parsing but
  no direct malformed-input proof for the fMP4 AVCC parser and inconsistent
  partial-result contracts across the three consumers.
- Status: done (`codec::parse_avcc_config` rewritten around an
  `Option`-returning `.get()?`-bounds-checked helper so truncation never
  yields a partial SPS-only/PPS-only prefix; the other two parsers were
  already fail-closed and gained adversarial regression tests for missing
  PPS-count byte, truncated PPS length/body, and maximal declared SPS length
  against a tiny buffer; 2026-07-18 by codex; Filed: 2026-07-17 by Q-002)

### Q-021 [resilience] [sonnet] Prove MPEG-TS packet and PES resource bounds
- Goal: `TsDemuxer::process_ts_packet` must ignore oversized adaptation
  fields and invalid PES header spans without corrupting current state, and
  the `MAX_PES_BUFFER` limit must remain effective under repeated
  continuation packets while a later valid PES still demuxes.
- Files: `src/media/mpegts.rs`, `src/media/mpegts_tests.rs`.
- Gates: break-it-first focused regressions; `scripts/build/resource-limit.sh
  cargo test media::mpegts::tests --lib`; relevant MPEG-TS demux/resync
  benchmark before/after if production code changes; `cargo fmt --all
  --check`; standard clippy and test gates.
- Context: Q-002 found a bounded corrupt-sync remainder regression but no
  direct proof for the separate 512 KiB PES accumulator or malformed
  adaptation/PES header recovery boundaries.
- Status: done (all three concerns were already handled safely by existing
  code — oversized adaptation fields are rejected before any state mutation,
  overrunning PES header spans append zero bytes and reset cleanly, and the
  512 KiB PES accumulator cap holds under a 6000-packet continuation flood
  with correct recovery afterward; tests-only change adding three
  hand-crafted-packet regressions; 2026-07-18 by codex; Filed: 2026-07-17 by
  Q-002)

### Q-022 [resilience] [sonnet] Reject non-finite and overflowing file start times
- Goal: `file_ingest::parse_start_time_ms` must reject `NaN`, infinities,
  float-to-millisecond overflow, and integer overflow in colon-delimited
  hours/minutes instead of coercing them to zero or saturated timestamps.
- Files: `src/media/file_ingest.rs`.
- Gates: break-it-first focused regression; `scripts/build/resource-limit.sh
  cargo test media::file_ingest::tests --lib`; `cargo fmt --all --check`;
  standard clippy and test gates.
- Context: Q-002 found ordinary negative/syntax rejection but no finite/range
  validation before floating-point casts and integer time arithmetic.
- Status: done (found a live bug, not just a coverage gap: `"nan"` silently
  became `0` and `"inf"` silently saturated to `i64::MAX` via Rust's
  float-to-int cast, and `hours * 3600 + minutes * 60` could overflow `i64`
  unchecked; fixed with a shared finite/range-checked `seconds_to_ms`
  helper and `checked_mul`/`checked_add` scaling, plus six adversarial
  regression tests; 2026-07-18 by codex; Filed: 2026-07-17 by Q-002)

### Q-002 [resilience] [haiku] Inventory crafted-bytes fault-injection coverage
- Goal: a table (in the journal + filed items) of every demux/parse entry
  point in `src/media/` vs the malformed-input cases it has tests for
  (truncated header, oversized declared length, invalid tag/type,
  non-monotonic timestamps, mid-stream parameter change).
- Files: read-only over `src/media/`; no code changes.
- Gates: none (inventory); each gap filed as a `[resilience] [sonnet]` item
  with exact entry-point function named.
- Context: `docs/testing-strategy.md` designates crafted-bytes unit tests as
  the home of fault injection after the in-memory edge subsystem was dropped.
  "No failure path may crash the engine" needs per-parser proof.
- Status: done (Mapped every media binary parser/demux owner plus adjacent
  string parsers; filed Q-018 through Q-022 for the non-duplicative gaps;
  2026-07-17 by codex; Filed: 2026-07-03 by bootstrap)

### Q-003 [performance] [sonnet] Seed the benchmark baseline ledger
- Goal: medians for `ring_buffer`, `avio_throughput`, and
  `high_performance_data_path` recorded in `baselines.md` with date, commit,
  and noise notes.
- Files: `docs/agent-guidance/quality/baselines.md`.
- Gates: three clean serial `scripts/build/resource-limit.sh cargo bench --bench <name>`
  runs on an otherwise idle host (`pgrep -x restream/mediamtx/ffmpeg` all
  empty).
- Context: Criterion state in `target/criterion/` is scratch; the ledger is
  the durable regression guard perf-sweep Mode A depends on. Without it every
  future comparison is blind.
- Status: in-progress (Claimed: 2026-07-18 by codex; Filed: 2026-07-03 by bootstrap)

### Q-011 [performance] [sonnet] Prove or reject RTMP video payload ownership transfer
- Goal: a measured runtime decision on replacing RTMP Raw video
  reuse-Vec-plus-`Bytes::copy_from_slice` with a handoff shape that moves
  converted payload ownership into `Bytes`, implemented or explicitly rejected
  with MSR/process-counter evidence.
- Files: `src/media/rtmp.rs`, `benches/codec_conversions.rs`,
  `docs/agent-guidance/quality/baselines.md`.
- Gates: baseline and after `scripts/build/resource-limit.sh cargo bench --bench
  codec_conversions -- 'codec/rtmp_payload_ownership'`; scoped
  `scripts/build/resource-limit.sh cargo test rtmp --lib`; bench-profile
  `MSR_OUTPUT_COUNTS=1200` receiver proof via MediaMTX `/v3/paths/list` plus
  `perf stat -p <restream-pid>` before/after; `cargo fmt --all --check`.
- Context: MSR perf at `da84fbe` showed RTMP egress, allocator calls, memmove,
  and `rml_rtmp::ChunkSerializer::serialize` in the remaining hot path. The
  micro-benchmark added in `6efa461` showed video handoff wins of 10-29% by
  avoiding the final copy into `Bytes`, while audio was noise. Do not repeat
  burst write coalescing (`b19cb17` rejected it).
- Status: done (Rejected at MSR runtime scale in this change. Filed:
  2026-07-12 by groom)

### Q-012 [performance] [opus] Evaluate CPU affinity for Tokio/SRT/RTMP thread families
- Goal: a measured decision on whether internal thread affinity/bin-packing
  reduces MSR CPU migrations and cache misses without hurting receiver health,
  implemented or rejected with evidence.
- Files: `src/main.rs`, `src/media/srt.rs`, `src/media/srt_egress.rs`,
  `docs/agent-guidance/quality/baselines.md`.
- Gates: process CPU mask/cgroup detection unit tests; before/after full
  `MSR_OUTPUT_COUNTS=1200` MediaMTX receiver proof; `perf stat -p` with
  migrations/cache/context-switch counters; `pidstat -t`; concurrency fast gate
  if thread creation or blocking handoff changes.
- Context: SRT muxer reuse and 2-worker Tokio policy cut CPU, but migrations
  remained high (`~230-337/sec` in 2026-07-12 MSR samples). Any pinning must be
  container-orchestration aware and derived from the effective CPU mask, not
  hard-coded host CPUs. Live external `taskset` partition probes showed the
  direction is promising only on a clean default-runtime run: SRT helpers on
  CPUs `0-1` and other Restream threads on `2-5` reduced CPU, cache misses,
  context switches, and migrations while preserving MediaMTX receiver health.
  A first in-process scanner prototype applied the intended masks but did not
  reproduce the CPU/cache/context-switch win, so do not add internal pinning
  without a stronger ownership-aware design and concurrency proof gates.
- Status: open, narrowed to an opt-in runtime affinity design (Filed:
  2026-07-12 by groom)

### Q-013 [efficiency] [sonnet] Test allocator arena limits for MSR memory plateau
- Goal: a single-variable MSR run proves whether `MALLOC_ARENA_MAX` or an
  allocator setting lowers RSS/PSS plateau without increasing CPU or receiver
  failures; record the decision in `baselines.md`.
- Files: `docs/agent-guidance/quality/baselines.md`,
  `docs/matrix-resource-constraints.md` if an operator-facing setting is
  recommended.
- Gates: before/after `MSR_OUTPUT_COUNTS=1200` run with MediaMTX
  `/v3/paths/list` receiver proof; `/proc/<pid>/smaps_rollup` RSS/PSS/private
  dirty samples; `perf stat -p` CPU/cache/context counters; no Restream
  warn/error/panic lines.
- Context: MSR memory evidence showed retained Rust payload below 50 MiB while
  RSS/PSS plateau was consistent with allocator/native arena retention. This is
  a safer first memory experiment than hot-path data-structure rewrites.
- Status: done (Rejected as a default operator setting in this change. Filed:
  2026-07-12 by groom)

### Q-004 [proof] [haiku] Panic-path inventory for src/media
- Goal: classified list of every `.unwrap()`, `.expect(`, `panic!`,
  `unreachable!` in non-test `src/media/` code: invariant-safe (with the
  one-line justification) vs fallible (filed as `[proof] [sonnet]` fix items).
- Files: read-only; output to journal + backlog.
- Gates: none (inventory).
- Context: proof-sweep discovery recipe; the engine no-crash contract makes
  every fallible panic path in the media runtime a latent broadcast outage.
- Status: done (2026-07-18) — inventory found one fallible panic site
  (`src/media/hls/preview.rs:21`, a TOCTOU race on the HLS preview
  consumer registry) and fixed it directly with a regression test rather
  than filing a follow-up; see journal 2026-07-18 07:40 Q-004 DONE and
  commit `f0aec2fe`. All other `.unwrap()`/`.expect(`/`panic!`/
  `unreachable!` sites in non-test `src/media/` code were invariant-safe.

### Q-005 [resilience] [sonnet] Baseline the harness fault modes
- Goal: current pass/fail state of `fault.resilience`, `fault.egress-retry`,
  `fault.output-stall`, and `recovery` recorded in the journal; any failure or
  flake filed as its own item with output attached.
- Files: none modified (measurement only).
- Gates: `scripts/build/resource-limit.sh cargo build --bin test_harness` then each
  mode via `scripts/build/resource-limit.sh target/debug/test_harness <mode>`,
  serially, idle host.
- Context: these modes are the live resilience contract; the loop needs a
  known-green baseline before it can treat a failure as a regression signal.
- Status: done (2026-07-18) — all four modes green on an idle host:
  `fault.resilience` 17/17, `fault.egress-retry` 4/4, `fault.output-stall`
  2/2, `recovery` 7/7; no failures or flakes found, nothing to file; see
  journal 2026-07-18 08:10 Q-005 DONE.

### Q-006 [efficiency] [sonnet] Seed the resource baseline table
- Goal: RSS, ring payload, and AVIO high-water marks from a
  `resource-sweep` harness run recorded in `baselines.md` next to the
  historical 2026-06-27 numbers.
- Files: `docs/agent-guidance/quality/baselines.md`.
- Gates: `scripts/build/bench-harness.sh` then
  `scripts/build/resource-limit.sh target/bench/test_harness resource-sweep`, serial,
  idle host.
- Context: the 2026-06-27 memory-optimization pass cut ~205 MB RSS across 15
  scale cases; without a refreshed baseline, regressions of that work are
  invisible.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-007 [groom] [sonnet] Diff the stage proof map against the fast gate
- Goal: every current rule claimed in `docs/stage-boundary-proof-map.md`
  mapped to its enforcement in `scripts/check/concurrency/fast.sh` /
  `scripts/check/concurrency/contract.sh`; uncovered rules filed as
  `[proof]` items (tier per rule complexity).
- Files: read-only; output to backlog + journal.
- Gates: none (grooming).
- Context: the proof map is the maintained human-readable inventory; the gates
  are what actually bind. Drift between them is unproven confidence.
- Status: done (2026-07-18) — audited all 10 boundary rows against
  `fast.sh`/`contract.sh`; the two rows that explicitly claim mandatory-gate
  coverage (runtime admission->registry, cancel/teardown->cleanup) are
  correctly enforced, and the other 8 rows document proof that intentionally
  lives in the general test suite per the Inner Loop routing table, not a
  gap. No stale or missing test names found. No new `[proof]` items filed;
  see journal 2026-07-18 08:35 Q-007 DONE.

### Q-008 [modularity] [sonnet] Execute the topmost undone layering-roadmap step
- Goal: the first not-yet-done step in `docs/layering-roadmap.md` completed at
  the lightest ladder rung, or (if already done in code) the roadmap updated
  to reflect reality.
- Files: per the roadmap step; plus `docs/layering-roadmap.md`.
- Gates: scoped `cargo test` for the touched area;
  `./scripts/check/api-contract.sh` if contract surface moved; standard
  quality-loop gates.
- Context: known cross-layer flows still open: planner→media backend parsing,
  runtime core emitting API-shaped JSON, protocol handlers reading raw SQL
  (`docs/layering-roadmap.md` § Current Shape).
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-009 [performance] [opus] Eliminate one copy in the AVIO→TsMux path
- Goal: FFmpeg AVIO output written directly into a pre-sized `BytesMut`,
  removing the AVIO buffer → `ts_accum` copy, with before/after
  `avio_throughput` + `high_performance_data_path` numbers and green
  `mixed.live.srt.h264.a1.bf0` / `mixed.live.rtmp.h264.a1.bf0` harness modes.
- Files: `src/media/avio.rs`, `src/media/srt.rs` (TS mux accumulator), ledger.
- Gates: perf-sweep Mode C discipline (baseline first, serial measurement),
  protocol correctness modes, concurrency gates if thread-hops move.
- Context: 2026-06-27 CPU profile: `memmove` 3.28% + `VecDeque::extend` 0.43%
  self-time from the two-copy path — the top standing optimization. Requires
  AVIO→TsMux interface redesign: opus tier, do not attempt below it.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-010 [efficiency] [opus] Evaluate pooling for per-packet Arc<MediaPacket> allocation
- Goal: a measured decision (implemented or explicitly rejected with numbers)
  on slab/pool allocation for `MediaPacket`, based on `_int_malloc` 0.87%
  self-time in the 2026-06-27 profile.
- Files: `src/media/ring_buffer.rs` and packet construction sites (survey
  first).
- Gates: perf-sweep Mode C; `ring_buffer` bench before/after; loom/unit proofs
  if ownership rules change.
- Context: pooling changes ownership semantics on the hot path — correctness
  risk outweighs the win unless proven; a documented rejection is a valid
  completion.
- Status: open (Filed: 2026-07-03 by bootstrap)

## Blocked

(none)

## Archive

(done items move here with their commit hashes)
