# SRT six-driver matrix and connected lifecycle audit (2026-08-18)

This record captures the post-handshake-retry differential matrix for the
native libsrt reference and all six Rust runtime drivers, plus the connected
listener/group lifecycle audit in restream.

## Contents

- [Build parity](#build-parity)
- [Matrix results](#matrix-results)
- [Resource cost per Mbps](#resource-cost-per-mbps)
- [Why the connected case was failing](#why-the-connected-case-was-failing)
- [Group lifecycle and remaining scope](#group-lifecycle-and-remaining-scope)

## Build parity

The host is x86_64 on an AMD EPYC with six logical CPUs. `/proc/cpuinfo`
contains all x86-64-v3 requirements: SSSE3, SSE4.1/4.2, POPCNT, AVX/AVX2,
BMI1/BMI2, F16C, FMA, MOVBE, and ABM/LZCNT.

The Rust measurement binaries were built by
`scripts/build/srt-interop-bench.sh` with `cargo build --profile bench`,
`opt-level=3`, thin LTO, and `-C target-cpu=x86-64-v3`. The production
measurement binaries were rebuilt by `scripts/build/bench-harness.sh` with
the same target CPU. The pinned libsrt reference was built from the local
static tree with `-O3 -march=x86-64-v3 -D_FORTIFY_SOURCE=3
-fstack-protector-strong -fPIC`, Release mode, and bonding enabled.

## Matrix results

The post-retry matrix used 10-second cells at 8 Mbps, 200 ms SRT latency,
loss levels of 0/5/10%, and netem delays of 0/50/100 ms. It exercised 63
cells: nine per implementation. Every caller and listener exited 0.

Artifact: `.local/artifacts/srt-six-driver-matrix-20260818-postretry.tsv`

| backend | cells | throughput | latency overhead ms | CPU ms/1k packets | peak RSS KiB | loss rate |
|---|---:|---:|---:|---:|---:|---:|
| libsrt | 9 | 99.7% | 15.48 | 701.57 | 6,158 | 5.18% |
| mio | 9 | 99.9% | 14.74 | 603.25 | 2,987 | 6.28% |
| tokio | 9 | 99.9% | 22.05 | 604.85 | 3,641 | 6.06% |
| smol | 9 | 99.9% | 21.11 | 829.32 | 3,143 | 6.36% |
| monoio | 9 | 99.9% | 16.84 | 498.72 | 3,228 | 5.60% |
| glommio | 9 | 99.9% | 29.33 | 675.81 | 14,592 | 5.73% |
| compio | 9 | 99.9% | 25.11 | 668.92 | 3,598 | 7.26% |

The separate no-loss bitrate matrix used 2/8/16 Mbps, again for 10 seconds
per cell. All 21 cells passed: 3 per implementation.

Artifact: `.local/artifacts/srt-six-driver-bitrate-20260818.tsv`

| backend | throughput | latency overhead ms | CPU ms/1k packets | peak RSS KiB | loss rate |
|---|---:|---:|---:|---:|---:|
| libsrt | 99.8% | 11.57 | 1,227.43 | 6,144 | 0.00% |
| mio | 99.7% | 5.36 | 1,086.53 | 3,285 | 0.00% |
| tokio | 99.8% | 6.87 | 1,027.34 | 3,456 | 0.00% |
| smol | 99.7% | 2.44 | 1,444.66 | 3,157 | 0.00% |
| monoio | 99.7% | 15.36 | 793.44 | 3,072 | 0.07% |
| glommio | 99.8% | 38.24 | 1,185.65 | 14,507 | 0.09% |
| compio | 99.7% | 13.07 | 994.01 | 3,328 | 3.16% |

The corresponding charts are generated in
`.local/artifacts/srt-six-driver-matrix-20260818-postretry-analysis/` and
`.local/artifacts/srt-six-driver-bitrate-20260818-analysis/`.

## Resource cost per Mbps

For the 8 Mbps matrix, these values average the nine loss/latency cells.
CPU is the combined caller plus listener process CPU rate; RSS is the larger
of caller/listener peak RSS. `cpu_ms_s_per_mbps / 1000` is CPU cores per Mbps.

| backend | achieved Mbps | CPU ms/s/Mbps | CPU cores/Mbps | RSS KiB/Mbps |
|---|---:|---:|---:|---:|
| libsrt | 8.422 | 66.501 | 0.066501 | 731.2 |
| mio | 7.990 | 57.303 | 0.057303 | 373.8 |
| tokio | 8.010 | 57.502 | 0.057502 | 454.5 |
| smol | 7.989 | 78.775 | 0.078775 | 393.4 |
| monoio | 8.014 | 47.359 | 0.047359 | 402.9 |
| glommio | 8.004 | 64.176 | 0.064176 | 1,823.1 |
| compio | 7.997 | 63.533 | 0.063533 | 450.0 |

These are harness process costs, not a claim that a production restream
pipeline scales linearly with Mbps. They show that the Rust implementations
are not disadvantaged by compiler settings; monoio is the lowest CPU cost in
this bounded slice, while glommio remains the clear memory outlier.

## Why the connected case was failing

The handshake has two different information boundaries:

1. INDUCTION identifies the caller socket ID, but native libsrt commonly does
   not put StreamID or GROUP there.
2. CONCLUSION carries StreamID and GROUP. The Rust listener stores those in
   the Core before the Core emits `Connected`, and sends the listener's local
   GROUP mirror in the conclusion response.

That means the first packet cannot be assigned by socket ID, StreamID, or
GROUP. The UDP tuple is the provisional route key. After CONCLUSION, the
connected Core has peer socket ID, peer StreamID, and peer GROUP, so the
worker can use the socket ID as the physical group-member identity and
`(group_id, normalized_stream_id)` as the logical group key.

The correct handoff sequence is:

```text
listener receives CONCLUSION
  -> Core reaches Connected and queues the final handshake response
  -> listener drains the response and removes the tuple from pending state
  -> worker creates and owns the connected UDP socket
  -> worker emits Connected with stream/group/socket metadata
  -> async admission authenticates the stream
  -> worker authorizes the leg as Single or adds it to ConnectedGroup
```

`Connected` at this boundary means transport-connected, not yet authorized
for media. Data that arrives before authorization remains bounded in the Core
event queue and is drained when the admission command completes. This is the
right point to hand off ownership: moving the Core before CONCLUSION loses
GROUP/StreamID context, while authorizing before the worker owns its socket
can race packet processing.

The concrete connected-path defects found in this loop were:

- handshake control packets were sent only once, so loss could strand the
  connection before `Connected`; the Core now retransmits the cached current
  handshake packet once per second, up to five retries, and accepts duplicate
  induction/conclusion packets;
- a connected route that failed from its timer path could become `Closed`
  without being removed, leaving stale tuple ownership; timer and
  authorization failure paths now remove the route and release the tuple;
- the retry test and route-release test lock both boundaries.

The retry regression passed in `crates/srt-protocol/tests/test_srt_connection.rs`.
The restream Rust ingest tests passed 5/5, and the optimized live scenario
`mixed.live.srt.h264.a1.bf0` passed all 16 outputs, the Rust sink probe,
HLS, recording, stage-sharing, and lifecycle cleanup with both
`RESTREAM_SRT_BACKEND=rust` and `HARNESS_SRT_SINK_BACKEND=rust`.

## Group lifecycle and remaining scope

The pinned libsrt reference marks a group member connected before group
acceptance and submits only the first connected member to the listener's
accept queue; later legs remain group-owned. The Rust path has equivalent
logical ownership: each physical leg is admitted once, then `ConnectedGroup`
deduplicates Broadcast data or promotes Backup members while the Tokio layer
maps all members to one logical ingest session.

The current bounded evidence therefore says the single-leg connected handoff
is correct and the Core/group metadata boundary is sufficient. The remaining
bonding proof is the broader production matrix: distinct source tuples,
Broadcast duplicate suppression, Backup promotion, disconnect/reconnect, and
worker-affinity stress under the three receiver scaling strategies. Those
tests must retain the tuple as the routing key until CONCLUSION and must verify
that every member's disconnect releases the tuple without deleting a live
logical group.
