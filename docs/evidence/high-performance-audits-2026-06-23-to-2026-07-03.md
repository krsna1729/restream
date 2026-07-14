# High-performance audit evidence — 2026-06-23 to 2026-07-03

> **Status: historical evidence.** This file preserves dated progress,
> allocation, layout, and benchmark observations removed from the maintained
> hot-path guide. Re-run the relevant benchmark before using a number for a
> current decision.

## Contents

- [Progress Log](#progress-log)
- [Hot-Path Layout Follow-Up Audit (2026-07-02)](#hot-path-layout-follow-up-audit-2026-07-02)
- [Per-Frame Allocation Audit (2026-06-23)](#per-frame-allocation-audit-2026-06-23)

## Progress Log

| Step | Status | Result |
|---|---|---|
| Baseline suite and roadmap | Complete in `e266608` | Added lookup, ring, fan-out, queue, and layout measurements |
| Clean benchmark builds | Complete in `205aae2` | Removed four test-harness warnings from benchmark compilation |
| Direct RTMP ingest handles | Complete in `5299db4` | Ring and byte-counter access fell from ~119 ns to ~7.3 ns, about 94% lower |
| Compact ring slots | Complete in `ad4ac9b` | Controlled pinned-CPU rerun: storage fell 256 KiB → 32 KiB, producer throughput was neutral, 32-packet consumer improved ~5.7%, and 500-reader fan-out improved ~9.6% |
| Burst ring primitives | Complete in `e0f33ac` | `push_batch()` improved 32-packet publication by ~15%; `pull_burst()` improved 8-packet consumption by ~17%. All 14 ring tests pass. Existing single-packet APIs remain the latency path |
| Burst adoption in internal stages | Complete in `95f2849` | HLS, recording, and transcoder feeders drain reusable 32-packet bursts. The primitive measured up to ~17% faster; module tests and all-target compilation pass |
| Bounded chunk queues | Pending | Not started |
| Batched AVIO queue writes | Complete in `10eaaf6` | One lock and notification per burst reduced the 32 × 1316-byte queue round trip from ~7.49 μs to ~3.84 μs, about 49% lower. Adopted by HLS, recording, and transcoder feeders |
| Shared package stages | Pending | Not started |
| Worker sharding and local pools | Pending | Not started |
| Batch metadata, prefetch, and vectorized-search refinement | Complete | TS resync and Annex B start-code scanning are vectorized using `memchr` (`find_annexb_start_codes` in `codec.rs` and `for_each_nal_raw` in `mpegts.rs`). Production TS resync uses `memchr` (64 KiB scan is ~631 ns). Annex B NAL scanning uses runtime-dispatched `memchr::memmem` at ~118.73 GiB/s, outperforming `wide` (~40.25 GiB/s) and `pulp` (~4.14 GiB/s) while avoiding complex custom target-feature configurations. |
| Zero-copy HLS segment finalization | Complete in `24fd309` | 8 MiB finalization fell from ~4.26 ms to ~347 ns by transferring `BytesMut` ownership, over 99.99% lower |
| Native shared HLS packaging | Complete in `a5d736f` | Replaced the FFmpeg queue plus two-OS-thread path with inline `TsMuxer`, a reusable accumulator, one shared segmenter per pipeline, demand-driven browser heartbeats, and persistent-output reference tracking |
| Native HLS cost benchmark | Complete in `a5d736f` | Mux-only cost is ~27–147 µs per second of content across 720p30 to 4K30 profiles; six-second mux/accumulate/store cost is ~0.23–2.75 ms. A twenty-segment window retains roughly twice the original ten-segment estimate |
| Lock-free stage telemetry | Complete through `c332c90` | Graph, health, and telemetry views now render through `engine_views.rs`, while transport/API call sites use `MediaEngine` façade helpers instead of reaching into nested registries directly. Benchmarked control-plane helper lookups stayed on par with or better than direct registry reads |
| Native MPEG-TS data-path audit | Complete in `abc558b` | New demux/mux path adds major opportunities in PID dispatch, reusable drains, PES construction, direct TS output, batch APIs, and SIMD-assisted resynchronization/NAL scanning |
| Reusable MPEG-TS demux drains | Complete in `a741faf` | Real 6.7 MB fixture replay improved from ~12.44 ms to ~11.36 ms, about 8.7%; all 15 MPEG-TS tests pass |
| Direct MPEG-TS PID dispatch | Complete in `a741faf` | 8192-entry PID table reduced pinned fixture replay from ~11.40 ms to ~8.54 ms, about 25.9%; throughput improved ~34.9% |
| Vectorized TS resync | Complete in `467e5c9` | 64 KiB corrupted prefix: portable `memchr` scan ~631 ns (~96.8 GiB/s), removed hand-written scanner ~907 ns, scalar scan ~16.4 µs. Production keeps stride verification after candidate search |
| Cumulative native demux | Complete in `ed40c91` | After all native MPEG-TS optimizations, 6.7 MB fixture replay runs in ~4.28 ms (1.45 GiB/s), down from original ~12.44 ms—65.6% lower end-to-end |
| Zero-copy PES muxer | Complete in `5eaedcd` | Stack-resident PES header + direct TS output eliminated payload copy and temp arrays; 6.7 MB mux time fell from ~880 µs to ~490 µs (45% faster, 6.8 → 12.3 GiB/s) |
| Native codec assembly | Complete in `8b03aad` | Matched pinned runs show the intended 4K HEVC decode/scale/H.264 encode chain at 2.49 s versus 5.45 s without FFmpeg x86 assembly, a 2.19× speedup; static setup now verifies assembly support |
| Cached SRT ingest byte counter | Complete in `4eb8ea6` | Cloned the `Arc<AtomicU64>` before the receive loop, replacing a per-receive `active_ingests.read().await` + HashMap lookup with a direct `fetch_add` |
| Cached egress byte counters | Complete in `a9c534f` | RTMP and SRT egress paths now cache `bytes_sent` counter before their send loops, replacing per-packet/batched `update_egress_bytes()` async lookups |
| Production transcoder-stage benchmark | Complete in `76d3969` | Replaced the fake `Bytes::clone()` benchmark with the exact FFmpeg `MemoryQueue` + custom-AVIO stage. Current `source` passthrough processes the 6.4 MiB fixture in ~26.8 ms (~238 MiB/s) |
| Actual decode/filter/encode transcoder | Missing | Resolution presets configure encoder metadata but currently remux original compressed packets; implement and benchmark the real decoder, scaler/filter graph, encoder, output demux, and ring publication path |
| Hoist burst-drain Vecs before loop | Complete | `transcoder.rs` and `h264_transcoder.rs` feeder loops fixed. Benchmark `burst_drain_alloc` (bench-dev, x86-64 Zen): `alloc_per_burst` ~2.79 µs vs `hoisted_clear` ~2.54 µs per 32-packet burst — **~9% faster**, ~250 ns saved per burst cycle. At 5 bursts/s per consumer the saving compounds across all egress stages (HLS, recording, SRT, transcoder feeders). |
| Custom AVIO teardown | Fixed in `76d3969` | Production-context benchmarking exposed a custom `AVIOContext` double-close. Contexts now remain owned by their wrappers, which detach `pb` before FFmpeg context destruction; repeated benchmark iterations complete cleanly |
| `MediaPacket` field reordering + `#[repr(C)]` | Complete | Without `#[repr(C)]`, rustc's greedy-alignment heuristic places `payload: Bytes` (32 bytes) first, pushing `media_type`/`is_keyframe`/`pts`/`dts` to ArcInner offsets 52–71, spanning two cache lines. With `#[repr(C)]` and the declared field order, all hot consumer fields (type dispatch, track routing, timestamps, payload ptr+len) land in cache line 0 (ArcInner bytes 0–63); only the Bytes Arc management fields land in cache line 1. `#[repr(u8)]` on `MediaType` and `PayloadFormat` guarantees 1-byte enum size. |
| `const` CRC-32/MPEG-2 table | Complete | Replaced `static OnceLock<[u32; 256]>` with `const CRC32_TABLE: [u32; 256]` computed at compile time. All operations (loops, bit shifts, conditionals) are valid in `const fn`. Eliminates the atomic acquire load on every PAT/PMT write (~500 ms interval). Table lives in `.rodata`, no first-call latency. |
| Sentinel `u8` for continuity counter and PMT version | Complete | `StreamInfo.continuity: Option<u8>` → `u8` with `CC_UNSET = u8::MAX` (valid CC values 0–15); `TsDemuxer.pmt_version: Option<u8>` → `u8` with `PMT_VER_UNSET = u8::MAX` (valid version 0–31). Removes the discriminant byte and the `Option`-unwrap branch on every TS packet processed. |
| BytesMut burst alloc in SRT shared muxer | Complete | `srt.rs` shared muxer replaced per-chunk `Bytes::copy_from_slice` (one `malloc+memcpy` per muxed packet) with a single `BytesMut::with_capacity(65536)` per burst, then `Bytes::slice()` for each chunk (refcount bump only, no malloc). Benchmark `ts_chunk_burst_alloc` (bench-dev, x86-64 Zen): `per_chunk_copy_from_slice` ~3.93 µs vs `burst_bytesmut_then_slice` ~2.23 µs per 32-chunk burst — **~43% faster**, ~1.7 µs saved per burst. |
| Batched external transcoder stdin writes | Complete | `external_transcoder.rs` now accumulates all feedable packets from a 32-packet ring burst into one stdin write while preserving packet/byte metrics via `record_in_batch`. Benchmark `data_path/burst_mux_write` (`--profile bench`, 2026-07-02): per-packet write ~33.0 µs vs batch accumulate write ~12.1 µs per 32-packet burst, about **63% lower** in the modeled queue/write path. |
| `TsMuxer::mux_packet_by_stream_idx` hot-path bypass | Complete | `TsPacketFeeder::extend_ts_for_packet` already computes `stream_idx` for `DtsEnforcer`; it now passes that index straight to a new `mux_packet_by_stream_idx()` entry point instead of letting `mux_packet()` redo an equivalent linear `(media_type, track_index)` scan. Benchmark `stage_feeder` (`--profile bench`, `--baseline before_stream_idx_bypass`, 2026-07-03): `single_packet/audio_raw_aac_200b` ~72.2 ns → ~46.2 ns (**~35.6% lower**, p<0.05); `burst/30_video_30_audio_packets` ~33.4 µs → ~33.4 µs (**~4.1% lower**, p<0.05); `single_packet/video_raw_h264_8k` and `multi_audio/64_audio_packets_16_tracks` showed no significant change; the unrelated `dts_enforcer` control group was flat, confirming the comparison methodology. |
## Hot-Path Layout Follow-Up Audit (2026-07-02)

The current layout audit did not find a justified custom-SIMD replacement for
the existing byte scanners. TS sync still routes through `memchr`, Annex B
start-code scanning still routes through `memchr::memmem`, and the remaining
data movement is dominated by short `copy_from_slice`/memset-style operations
that the compiler and libc already lower efficiently. New SIMD work should wait
for profiler evidence on a named workload.

The high-confidence layout/cache-efficiency follow-ups are:

- ~~Add and benchmark a `TsMuxer::mux_packet_by_stream_idx` path.~~ Done
  2026-07-03: `TsPacketFeeder::extend_ts_for_packet` now passes the `stream_idx`
  it already computes for `DtsEnforcer::enforce` straight into a new
  `TsMuxer::mux_packet_by_stream_idx` entry point instead of letting
  `mux_packet` redo an equivalent linear scan. See the Progress Log entry
  above and the P2 note below for how this differs from the earlier rejected
  lookup-table experiment.
- Combine the two `audio_tracks` scans in `TsPacketFeeder::extend_ts_for_packet`:
  one scan gets sample rate/channels and a second scan gets stream index.
- Split `MuxStreamConfig` hot lookup fields (`media_type`, `track_index`, `pid`,
  `stream_type`) from cold metadata (`sample_rate`, `language`) if benchmarks
  show the per-packet lookup remains visible after stream-index bypassing.
- Move cold `StreamInfo` metadata such as language/title out of the MPEG-TS
  demuxer per-packet stream state if high-bitrate SRT ingest profiles show cache
  pressure in `TsDemuxer`.
- Keep `DtsEnforcer::last_dts` as a `Vec<i64>` unless a new benchmark says
  otherwise. A 2026-07-02 inline-array trial improved a too-small 32-stream
  version, but that capacity cannot cover the existing video+32-audio case; the
  safe 64-stream version regressed `stage_feeder/dts_enforcer` by roughly 8-11%.
  `TsMuxer::last_dts_90k` and `TsMuxer::continuity` remain possible candidates,
  but need the same benchmark-first treatment.
- Cache or stack-build PMT descriptor sections if keyframe/table insertion
  allocations show up in multi-output profiles.

The structures that should stay as-is unless benchmarks say otherwise:

- `MediaPacket` uses `#[repr(C)]` with hot dispatch/routing/timestamp/payload
  fields in the first cache line.
- `RingBuffer` keeps writer-owned atomics on separate 64-byte cache lines and
  deliberately keeps slots compact to reduce reader working set size.
- `TsDemuxer` PID dispatch uses the 8192-entry direct table, avoiding HashMap
  lookups on every TS packet.
- `StageMetrics` is a compact relaxed-atomic counter block with one writer per
  stage in normal operation; no padding should be added without a false-sharing
  benchmark.
## Per-Frame Allocation Audit (2026-06-23)

Measured allocations on the hot path per video frame (~1080p30, H.264, RTMP
ingest). "Warm" = after the first few frames when internal buffers have grown to
steady-state. All counts assume `PayloadFormat::Raw` egress (most common after
transcoder); FLV egress adds the AVCC→Annex B conversion Vecs.

### Ingest

| Stage | Per-packet allocs | Notes |
|---|---|---|
| RTMP socket → `rml_rtmp` | 1 (payload `Bytes`) | Library owns heap; `MediaPacket` borrows the ref |
| `ring.push(packet)` | 1 (`Arc<MediaPacket>` ~40 B) | Evicts old `Arc` on slot overwrite |
| **SRT → `TsDemuxer`** | **1 (frame copy)** | **Fixed (2026-06-23): was 3–8 (PES buf regrow)**. `flush_pes` now uses `Bytes::copy_from_slice` + `reset()` so the `Vec` capacity is retained across frames. |
| `push_batch` to ring | 1 `Arc` per packet | Same as RTMP |

**PES buffer fix detail**: `flush_pes` previously called `std::mem::take(&mut pes.buf)` which transferred the `Vec` to `Bytes::from()` (zero-copy) but left a zero-capacity `Vec` behind. The next frame restarted from capacity 0, triggering 3–8 `realloc` calls (doubling from 0→1→2→4→...→frame_size). For a 200 KB IDR that was ~8 reallocations per frame. The fix: `Bytes::copy_from_slice(&pes.buf)` (one allocation of exactly frame_size bytes) + `pes.reset()` (clears length, **preserves capacity**). Net: same 1 allocation per frame but 0 realloc cascades.

### Egress — per output per video frame

| Consumer | Format | Allocs | Notes |
|---|---|---|---|
| RTMP egress | Raw→FLV | 1 large (AVCC copy) + 2 small (NALU position Vecs) | `video_for_rtmp_into` → `annexb_to_avcc_into` → `split_annexb_nalus`. AVCC output is unavoidable (RTMP library needs to own it). 2 small Vecs ~48 B each. |
| RTMP egress | Flv→FLV | 0 | FLV passthrough: `payload.clone()` = `Arc` refcount only |
| SRT/HLS egress | Raw | 0 | `video_for_ts_into` returns `&payload` directly (zero-copy). `TsMuxer::output` pre-allocated, reused. |
| SRT/HLS egress | Flv→Raw | 2 small | `avcc_to_annexb_into` → `split_annexb_nalus`: 2 Vecs ~48 B each. Written into `video_conv_buf` (no extra alloc). |
| Recording | same as HLS | 2 small or 0 | |
| H264-transcoder feed | Flv→Raw | 0 | Migrated to `_into` variants (2026-06-23). |

### `annexb_to_avcc` scratch variant

`annexb_to_avcc_with_scratch(data, out, sc_scratch)` eliminates both small Vecs
by reusing a caller-provided `Vec<(usize,usize)>`. Benchmarked 2026-06-23:

| Input | `two_pass` | `with_scratch` |
|---|---|---|
| P-frame 8 KiB, 1 NALU | 2.73 µs | **1.80 µs (+34%)** |
| P-frame 30 KiB, 3 NALU | 9.83 µs | **8.95 µs (+9%)** |
| IDR 80 KiB, 1 NALU | **16.98 µs** | 24.07 µs (-42%) |

**Current production choice: `two_pass`** (wins for dominant large IDR case). Re-evaluate if workload shifts to many small NALUs.

### Unbounded allocation risks

| Structure | Bound | Location |
|---|---|---|
| `MemoryQueue::VecDeque<u8>` | Bounded to 2 MB (steady-state ≈ 1.5 MB at 50 Mbps/250 ms) | `src/media/avio.rs`; 2 per transcoder |
| `TsMuxer::output: Vec<u8>` | largest TS burst per frame ≈ `frame_size / 1316 × 188` bytes | per consumer; stabilises at IDR size |
| `PesAccumulator::buf` | `MAX_PES_BUFFER` constant in `mpegts.rs` | per stream per demuxer |
| `TsDemuxer::remainder` | `TS_PACKET_SIZE` = 188 bytes | per demuxer |
| `sps_pps_cache: Vec<u8>` | SPS+PPS size ≈ 50 bytes | per consumer |
| `HLS accumulator: BytesMut` | segment size ≈ bitrate × 6s ≈ 18 MB at 24 Mbps | shared across all HLS outputs per pipeline |
| `IngestSecurityService` HashMap | `tracked_ip_limit` (default 10 000 entries) | enforced since 2026-06-23 fix |

All structures are bounded. `MemoryQueue` is the largest steady-state allocation
and is proportional to stream bitrate × transcoder latency.
