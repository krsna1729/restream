---
name: bench
description: Run a Criterion benchmark suite for this repo. Use before and after any hot-path change (src/media, ring buffers, mux/demux loops, AVIO queues, SRT/RTMP packet loops, HLS segmenting, transcoder paths) to measure impact, or when asked to "benchmark", "measure performance", or compare before/after numbers.
---

# Skill: bench

Run a Criterion benchmark suite for this repo. Use before and after a hot-path change to measure impact.

## Usage

`/bench <bench_name>`

Benchmark names come from the `[[bench]]` declarations in `Cargo.toml`; the
implementations under `benches/` describe the measured production path. Do not
copy the current target list into this skill.

## Steps

1. If no argument is given, read the `[[bench]]` declarations in `Cargo.toml`,
   map them to the changed production path, and ask which relevant target to
   run if more than one remains plausible.
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
