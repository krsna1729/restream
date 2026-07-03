---
name: protocol-test
description: Run the live protocol correctness matrix (RTMP/SRT/HEVC harness modes) and the bounded media validation suite. Use after changes to RTMP, SRT, HLS, or mux/demux logic, or when asked to validate protocol behavior end to end.
---

# Skill: protocol-test

Run the live protocol correctness matrix and media validation suites. Use after changes to RTMP, SRT, HLS, or mux/demux logic.

## Steps

1. Preflight: confirm no live pipeline is running (`pgrep -x restream`, `pgrep -x mediamtx`, `pgrep -x ffmpeg` all empty). Build the harness: `scripts/resource-limit cargo build --bin test_harness`.

2. Run the protocol correctness modes relevant to the change (each is one harness invocation):
   ```sh
   scripts/resource-limit target/debug/test_harness correctness-rtmp
   scripts/resource-limit target/debug/test_harness correctness-srt
   scripts/resource-limit target/debug/test_harness correctness-srt-rtmp
   ```
   For HEVC-affecting changes add `correctness-hevc-rtmp` and `correctness-hevc-srt`.
   For B-frame/timestamp changes add `bframe-rtmp`.
   For SRT encryption/policy changes add `correctness-srt-policy` and `srt-crypto-matrix`.
   - If a mode fails: report failures and ask whether to continue to media validation.

3. Run the bounded media validation suite: `scripts/resource-limit ./test/run-media-validation.sh`

4. Report a summary of all runs: pass/fail counts, any failures with their output.

## Notes
- Live modes run in a private loopback namespace by default. Only pass `--no-netns` if the user explicitly requests host networking.
- For SRT-specific changes, also run `/media-test srt` (scoped unit tests) before this suite.
- For RTMP-specific changes, also run `/media-test rtmp` first.
- These tests can take several minutes. Report progress as each mode completes.
- Correctness modes may overlap when isolated; measurement modes (bench, bitrate-sweep, resource-sweep) must stay serial and use the bench-profile harness (`scripts/build-bench-harness.sh` → `target/bench/test_harness`).
