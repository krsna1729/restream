# Concurrency Proof Coverage Report - 2026-07-02

Baseline: `0efa0d4` on `feat/rust-backend-rewrite-v2` after merging the proof work from the isolated `codex/proof-*` branches.

This report summarizes the model, property, unit, and live-harness proof surface for concurrency primitives and thread/process hop boundaries. It is intentionally proof-oriented rather than line-coverage-oriented.

## Summary

| Boundary | Proof type | Mandatory gate coverage |
|---|---|---|
| Ring seal/migration/read ordering | Loom + proptest + deterministic tests | `ring_migration_loom`, `prop_no_loss_no_gap_no_duplication`, `prop_multi_reader_migration_preserves_each_reader_order` |
| TS chunk ring wait/cancel/live readers | Loom + unit tests | `ts_chunk_ring_loom`, `live_reader_starts_after_existing_chunks` |
| AVIO/MemoryQueue close/wake/backpressure | Loom + unit/proptest tests | `avio_loom`, `media::avio::tests`, `write_batch_round_trips_random_chunks` |
| Stage registry replacement and TS muxer sweep | Loom + lifecycle unit tests | `transcoder_stage_loom`, `ts_muxer_stage_loom`, stale attempt tests |
| External transcoder pipe/output/SRT path | Unit + proptest + focused live harness | external transcoder marker tests, DTS routing proptest, `mixed.asset.file.h264.a1` smoke |
| Internal transcoder/libavcodec timestamp and metadata continuity | Unit + proptest + loom | chunked remux timestamp-order test, source-stage DTS chunking proptest, codec metadata replacement loom |
| Recording TS -> MP4 -> TS remux timestamp continuity | Deterministic round-trip unit tests | `remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled`, `remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled` |
| SRT protocol boundaries | Unit/stress tests | stream-id normalization tests, sender semaphore tests, `epoll_waiter_coordination` |
| Child process lifecycle and cleanup | Static script guard + unit test + live contract cleanup checks | `kill_and_wait_child_terminates_spawned_process`, process lifecycle guard, post-harness orphan checks |
| Runtime status after cleanup/recovery | API/status tests + live harness | API lifecycle tests, `fault.resilience`, `fault.egress-retry`, `fault.output-stall`, `recovery` |

## New Proof Coverage Added In This Sweep

### Ring And TS Chunk Ring

- `tests/ring_migration_loom.rs`
  - Added multi-reader seal wake coverage: a seal must wake all blocked readers, not only one waiter.
- `tests/ring_migration.rs`
  - Added `prop_multi_reader_migration_preserves_each_reader_order`, covering two readers with different pre-seal drain positions and checking per-reader ordering through migration.
- `src/media/ts_chunk_ring.rs`
  - Added `TsChunkReader::new_live` and `live_reader_starts_after_existing_chunks`, proving live readers skip existing buffered TS chunks and consume only future chunks.

### AVIO / Memory Queue

- `tests/avio_loom.rs`
  - Added loom coverage for batch writers blocked on backpressure.
  - Added close/read wake coverage for batch writer paths.
- `src/media/avio.rs`
  - Existing unit/proptest coverage remains the deterministic gate for read/write/batch behavior and poisoned-lock recovery.

### Stage Lifecycle

- `tests/transcoder_stage_loom.rs`
  - Added cleanup/replacement atomicity coverage for stage registry state.
  - Added codec metadata continuity coverage so cancelled stage replacement preserves codec-hint contract under concurrent creators.
- `tests/ts_muxer_stage_loom.rs`
  - Added sweep-vs-reader-registration coverage using a loom-compatible liveness model.

### Internal Transcoder / libavcodec Path

- `tests/transcoder.rs`
  - Added `internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order`, proving in-process decode/scale/encode output DTS remains monotonic when input TS is delivered in irregular queue chunks.
  - Added `prop_source_stage_chunked_input_preserves_per_stream_dts_order`, checking per-stream DTS monotonicity and non-negative PTS/DTS across randomized queue chunk boundaries in the internal source-stage demux path.
  - Added `replacement_video_stage_preserves_codec_hint_and_audio_tracks`, proving replacement stage creation after cancellation preserves codec hint and audio-track metadata contract.

### Critical External Transcoder / SRT Path

- `src/media/external_transcoder.rs`
  - Added testable `external_output_stream_idx` routing.
  - Added deterministic routing coverage ensuring known audio tracks map to distinct DTS streams and unknown/disabled audio does not alias to video or the first audio track.
  - Added `proptest_external_output_dts_routing_preserves_per_stream_monotonicity`, covering random audio-track permutations and mixed packet sequences.
  - Existing marker-fixture checks cover file-mode transcode control and live external-stage output.
- `src/media/srt.rs`
  - Shared SRT TS muxers and SRT egress readers attach at the live edge to avoid replaying stale ring/chunk backlog to live consumers.
- `src/media/ts_chunk_ring.rs`
  - `TsChunkReader::new_live` backs the SRT egress live-edge proof.

### Recording / HLS Timestamp Boundaries

- `src/media/hls.rs`
  - Added `hls_segment_boundaries_preserve_non_decreasing_dts_per_stream`, a deterministic in-memory proof that demuxed DTS values stay non-decreasing per stream across consecutive HLS MPEG-TS segment boundaries.
  - Coverage includes both packet-level DTS monotonicity and explicit first-packet-vs-previous-segment-last boundary checks after HLS keyframe-triggered segmentation.
- `src/media/recording.rs`
  - Added `remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled` and `remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled`, closing the previously open recording-remux-continuity gap.
  - Each test demuxes the checked-in canonical TS fixture with the codebase's own `TsDemuxer` (no dependency on the external `ffprobe` binary), runs the production TS -> MP4 remux path, remuxes the resulting MP4 back to TS via the bundled ffmpeg, and demuxes that round-trip TS with the same `TsDemuxer`.
  - Asserts per-stream DTS is monotonically non-decreasing with no inter-packet gap over 1s (catching dropped GOPs/frames) on both the source TS and the roundtrip TS, and that video/audio timeline span is preserved within 40ms across the full TS -> MP4 -> TS trip, under both `retain_source_ts` permutations.

### SRT Protocol Boundaries

- `src/media/srt.rs`
  - Equivalent percent-encoded and literal SRT stream IDs normalize to the same key before auth/duplicate registration checks.
  - The libsrt listener policy callback is panic-contained instead of unwinding across the C callback boundary.
  - Sender semaphore acquisition routes through a production helper exercised by existing semaphore tests.

### Process Lifecycle / Harness

- `src/bin/test_harness.rs`
  - Added `kill_and_wait_child` coverage through `tests::kill_and_wait_child_terminates_spawned_process`.
  - Extended `fault.output-stall` with a sibling-isolation subtest (`rtmp-stalled-sink-isolation-under-many-outputs`) that keeps one RTMP sink intentionally non-draining while sibling RTMP outputs drain through generalized sink servers.
  - The subtest now proves isolation: the stalled output surfaces `status=stalled` while sibling outputs remain `running`/progressing with positive bytes and fresh progress timestamps.
  - `FAULT_OUTPUT_STALL_SIBLINGS` controls sibling fanout (default `12`), capped by `N_PER_GROUP` so `N_PER_GROUP=1` keeps the mode cheap for fast loops.
- `scripts/build/resource-limit.sh`
  - Honors `RESTREAM_BUILD_LOCK_FILE` and rejects relative paths.
- `scripts/check/concurrency/contract.sh`
  - Defaults a host-global build lock when unset.
  - Adds static lifecycle guards for child process handling.
  - Captures a runtime-process baseline and checks that harness-mode cleanup
    leaves no new `restream`, `mediamtx`, `ffmpeg`, `ffprobe`, or
    `test_harness` survivors behind.

## Gate Inventory

### Fast Proof Gate

`bash ./scripts/check/concurrency/fast.sh` runs:

- Loom targets:
  - `avio_loom`
  - `ring_migration_loom`
  - `ts_chunk_ring_loom`
  - `ts_muxer_stage_loom`
  - `transcoder_stage_loom`
- Focused API/status lifecycle tests.
- Ring migration property tests: `prop_no_loss_no_gap_no_duplication` and `prop_multi_reader_migration_preserves_each_reader_order`.
- AVIO unit/property tests: `media::avio::tests` and `write_batch_round_trips_random_chunks`.
- SRT stress and protocol-boundary tests: `epoll_waiter_coordination`, stream-ID normalization, and sender semaphore checks.
- External transcoder routing, DTS, and H264 marker-fixture checks.
- Internal transcoder timestamp, chunking, and replacement metadata checks.
- HLS MPEG-TS segment DTS boundary check.
- Recording TS -> MP4 -> TS remux timestamp-continuity checks under both source-retention permutations.
- Process lifecycle and slow-sink sibling-count harness unit checks.
- Ingest/egress lifecycle proptests.

### Full Contract Gate

`bash ./scripts/check/concurrency/contract.sh` runs everything in the fast proof gate plus:

- `scripts/check/history-grouping.sh`
- static process lifecycle guards
- debug binary build for `restream` and `test_harness`
- live harness modes:
  - `fault.resilience`
  - `fault.egress-retry`
  - `fault.output-stall`
  - `recovery`
- post-mode orphan process checks for any new `restream`, `mediamtx`, `ffmpeg`,
  `ffprobe`, or `test_harness` processes started by the gate

## Validation Performed During The Sweep

The following focused checks passed serially after merging the isolated proof branches:

```sh
cargo fmt --all --check
bash -n scripts/build/resource-limit.sh scripts/check/concurrency/contract.sh
RESTREAM_BUILD_LOCK_FILE=relative scripts/build/resource-limit.sh true # expected exit 2
./scripts/harness/loom-target.sh ring_migration_loom
./scripts/harness/loom-target.sh avio_loom
./scripts/harness/loom-target.sh transcoder_stage_loom
./scripts/harness/loom-target.sh ts_muxer_stage_loom
scripts/build/resource-limit.sh cargo test internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order --test transcoder -- --nocapture
scripts/build/resource-limit.sh cargo test prop_source_stage_chunked_input_preserves_per_stream_dts_order --test transcoder -- --nocapture
scripts/build/resource-limit.sh cargo test replacement_video_stage_preserves_codec_hint_and_audio_tracks --test transcoder -- --nocapture
scripts/build/resource-limit.sh cargo test prop_multi_reader_migration_preserves_each_reader_order --test ring_migration -- --nocapture
scripts/build/resource-limit.sh cargo test media::avio::tests --lib -- --nocapture
scripts/build/resource-limit.sh cargo test srt_stream_ids_normalize_plain --lib -- --nocapture
scripts/build/resource-limit.sh cargo test srt_stream_ids_keep_slashes_as_literal_key_data --lib -- --nocapture
scripts/build/resource-limit.sh cargo test srt_sender_semaphore --lib -- --nocapture
scripts/build/resource-limit.sh cargo test --bin test_harness tests::kill_and_wait_child_terminates_spawned_process -- --exact --nocapture
N_PER_GROUP=1 scripts/build/resource-limit.sh cargo run --bin test_harness -- fault.output-stall --no-netns
env N_PER_GROUP=1 ONLY_CHECKS=ffprobe SKIP_LOAD=1 scripts/build/resource-limit.sh cargo run --bin test_harness -- mixed.asset.file.h264.a1
```

The full live `scripts/check/concurrency/contract.sh` gate remains the sign-off gate for broad lifecycle changes, but it should be run serially on a stable host because it starts several live harness modes.

After wiring the expanded proof labels into `scripts/check/concurrency/common.sh`, both `bash ./scripts/check/concurrency/fast.sh` and `bash ./scripts/check/concurrency/contract.sh` passed. The full contract run produced logs for all mandatory proof steps and live modes under `.local/artifacts/concurrency-contract-logs/`, and the post-run process cleanup check found no new `restream`, `mediamtx`, `ffmpeg`, `ffprobe`, or `test_harness` survivors beyond the gate's startup baseline.

## Remaining Gaps

- The full contract gate is intentionally heavier than the focused checks above; continue running it before final sign-off when host resources allow.
- Slow-sink sibling isolation now has focused harness coverage at low output counts; broader high-output soak coverage remains a separate, resource-heavy confidence run.
- Internal transcoder/libavcodec timestamp and metadata continuity now has unit, proptest, and loom coverage; live multi-codec soak coverage remains a separate confidence run.
- HLS segment-boundary DTS monotonicity is now covered in memory, and recording remux continuity (TS -> MP4 -> TS timestamp continuity under source-retention permutations) is now covered by a dedicated proof test (`src/media/recording.rs`, wired into the fast proof gate).
