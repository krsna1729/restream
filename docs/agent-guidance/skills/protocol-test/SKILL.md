---
name: protocol-test
description: Validate restream RTMP/SRT/HLS, codec, mux/demux, or live fault-recovery behavior using the harness catalog and bounded media suite.
---

# Protocol Test

Run relevant scoped unit tests first (`av_sync` for timestamps). Follow
[AGENTS.md](../../../../AGENTS.md) for host/build safety, then prepare and inspect
the canonical harness:

```sh
scripts/harness/run.sh --prepare
target/bench/test_harness catalog help
target/bench/test_harness catalog plan <mode>
scripts/harness/run.sh <mode>
```

Choose scenarios whose plans cover the changed protocol, codec, timestamp,
encryption, audio-track, or recovery contract. Use the live catalog rather than
a copied list of mode names. Run `scripts/harness/media-validation.sh` for
cross-codec/mux changes or when the bounded validation suite is requested.

Private loopback namespaces are the default; use `--no-netns` only when
required and report why. Correctness scenarios may overlap when isolated;
measurements must run serially.

Investigate a relevant failure before broadening the suite. Fix within the
authorized task or report the blocker and remaining verification; a failed
check is not itself a reason to ask permission to continue useful work.
Report commands, results, and uncovered behavior.
