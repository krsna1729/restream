---
name: bench
description: Run a Criterion benchmark suite for this repo. Use before and after any hot-path change (src/media, ring buffers, mux/demux loops, AVIO queues, SRT/RTMP packet loops, HLS segmenting, transcoder paths) to measure impact, or when asked to "benchmark", "measure performance", or compare before/after numbers.
---

# Skill: bench

Run a Criterion benchmark suite for this repo. Use before and after a hot-path change to measure impact.

## Usage

`/bench <bench_name>`

Available bench names (from `benches/`):
- `ring_buffer` — ring buffer throughput
- `avio_throughput` — AVIO queue throughput
- `high_performance_data_path` — end-to-end data path
- `hls_cost` — HLS segmentation cost
- `hls_fmp4_cost` — HLS fMP4 segmentation cost
- `matrix_throughput` — matrix/mux throughput
- `srt_ingest_latency` — SRT ingest latency
- `transcoder_throughput` — transcoder throughput
- `stage_feeder`, `stage_metrics` — stage pipeline
- `codec_conversions` — codec conversion costs
- `simd_alternatives` — SIMD vs scalar comparison
- `alert_tracker` — alert tracking overhead

## Steps

1. If no argument given, list available bench names and ask which to run.
2. Kill any live pipeline first (WSL2 memory safety): confirm `pgrep -x restream`, `pgrep -x mediamtx`, `pgrep -x ffmpeg` are all empty before building.
3. Build with bench profile: `scripts/build/resource-limit.sh cargo build --profile bench`
4. Run: `scripts/build/resource-limit.sh cargo bench --bench <name>`
5. Report the Criterion summary (throughput, latency, change % if a baseline exists).

## Workflow for before/after comparison

When measuring a hot-path change:
1. Run `/bench <name>` **before** making the change — note the baseline numbers.
2. Make the change.
3. Run `/bench <name>` again — Criterion will compare against its saved baseline automatically.

## Notes
- Never use `--release`; `--profile bench` shares the same opt-level with incremental compilation.
- Criterion saves baselines in `target/criterion/`; they persist across runs.
- Measurement runs must be serial: never run benches while another build, test, or harness run is active anywhere on this host.
- AGENTS.md requires benchmarking before and after any hot-path change in `src/media/`, ring buffers, mux/demux loops, AVIO queues, SRT/RTMP packet loops, HLS segmenting, or transcoder data paths.
- Durable medians belong in `docs/agent-guidance/quality/baselines.md`; update the ledger when a change intentionally shifts a number.
