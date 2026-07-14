---
name: protocol-test
description: Run the live protocol correctness matrix (RTMP/SRT/HEVC harness modes) and the bounded media validation suite. Use after changes to RTMP, SRT, HLS, or mux/demux logic, or when asked to validate protocol behavior end to end.
---

# Skill: protocol-test

Run the live protocol correctness matrix and media validation suites. Use after changes to RTMP, SRT, HLS, or mux/demux logic.

## Steps

1. Preflight: confirm no live pipeline is running (`pgrep -x restream`,
   `pgrep -x mediamtx`, `pgrep -x ffmpeg` all empty). Build the canonical
   harness with `scripts/build/bench-harness.sh`.

2. Inspect the current catalog and select the narrowest scenarios whose plans
   cover the changed protocols, codecs, timestamp shape, encryption policy, or
   multi-audio behavior:
   ```sh
   target/bench/test_harness catalog self-check
   target/bench/test_harness catalog list-modes
   target/bench/test_harness catalog plan <mode>
   scripts/harness/run.sh <mode>
   ```
   - If a mode fails: report failures and ask whether to continue to media validation.

3. Run the bounded media validation suite: `scripts/build/resource-limit.sh ./scripts/harness/media-validation.sh`

4. Report a summary of all runs: pass/fail counts, any failures with their output.

## Notes
- Live modes run in a private loopback namespace by default. Only pass `--no-netns` if the user explicitly requests host networking.
- For SRT-specific changes, also run `/media-test srt` (scoped unit tests) before this suite.
- For RTMP-specific changes, also run `/media-test rtmp` first.
- These tests can take several minutes. Report progress as each mode completes.
- Correctness modes may overlap when isolated; measurement modes (bench, bitrate-sweep, resource-sweep) must stay serial and use the bench-profile harness (`scripts/build/bench-harness.sh` → `target/bench/test_harness`).
