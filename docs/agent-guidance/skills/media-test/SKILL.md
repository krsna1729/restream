---
name: media-test
description: Run scoped media pipeline tests by module/filter (ring_buffer, srt, rtmp, avio, av_sync…), then optionally escalate to the live harness. Use after changing src/media/ code or when asked to test a specific media module. Follows the repo philosophy — scoped first, broader only when scoped passes.
---

# Skill: media-test

Run scoped media pipeline tests, then optionally escalate to live harness tests. Follows the AGENTS.md testing philosophy: scoped first, broader only when scoped passes.

## Usage

`/media-test <module_or_filter>`

`<module_or_filter>` is a test name filter passed to `cargo test`. Examples:
- `/media-test ring_buffer` — tests containing "ring_buffer"
- `/media-test srt` — tests containing "srt"
- `/media-test avio` — tests containing "avio"

## Steps

1. Preflight: confirm no live pipeline is running (`pgrep -x restream`, `pgrep -x mediamtx`, `pgrep -x ffmpeg` all empty) before any cargo command — WSL2 memory safety.

2. Run `scripts/resource-limit cargo test <filter>` where `<filter>` is the argument provided.
   - If this fails: report the failures clearly and **stop**. Do not escalate.
   - If no argument given: ask the user which module or filter to use.

3. If step 2 passes, report the scoped results, then ask: "Scoped tests pass. Escalate to the live harness? This takes several minutes." Escalation options:
   - `scripts/resource-limit target/debug/test_harness correctness` — general live correctness
   - `scripts/resource-limit target/debug/test_harness correctness-srt` / `correctness-rtmp` — protocol-scoped
   - `scripts/resource-limit target/debug/test_harness mixed-anchor` — scale/integration anchor

4. If the user confirms, build the harness first (`scripts/resource-limit cargo build --bin test_harness`) and run the chosen mode.

## Notes
- For timestamp/AV-sync changes use filter `av_sync`.
- For SRT changes use `srt` filter and also consider `/protocol-test`.
- For RTMP changes use `rtmp` filter and also consider `/protocol-test`.
- Never run live harness modes before unit tests pass.
- Integration/live modes use a private loopback namespace by default; use `--no-netns` only when required.
- Read `docs/media-pipeline.md` and `docs/testing.md` before making changes to `src/media/`.
