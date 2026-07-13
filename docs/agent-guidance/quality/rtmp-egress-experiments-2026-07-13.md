# RTMP Egress Experiments - 2026-07-13

## R1 serializer construction microbench

Added `benches/rtmp_serializer.rs` to compare current `rml_rtmp`
`ChunkSerializer::serialize` against a byte-for-byte equivalent direct
`serialize_into` prototype. The prototype writes chunk headers and payload
offsets directly into a reused `Vec`, avoiding the temporary chunk-slice vector
and fresh output allocation. Every benchmark case asserts byte parity with
`rml_rtmp` before measuring.

Representative medians from `scripts/build/resource-limit.sh cargo bench --bench
rtmp_serializer -- --quiet`:

| Payload | Chunk | rml owned Vec | direct reused Vec | Result |
|---|---:|---:|---:|---:|
| audio 200 B | 4 KiB | 10.712 us | 3.684 us | 2.9x faster |
| video P 8 KiB | 4 KiB | 31.488 us | 15.519 us | 2.0x faster |
| video P 30 KiB | 4 KiB | 139.980 us | 83.734 us | 1.7x faster |
| video IDR 80 KiB | 4 KiB | 422.390 us | 252.960 us | 1.7x faster |
| video P 30 KiB | 16 KiB | 95.481 us | 61.851 us | 1.5x faster |
| video IDR 80 KiB | 64 KiB | 320.860 us | 168.590 us | 1.9x faster |

Conclusion: a real `serialize_into` API in `rml_rtmp` is worth pursuing, but it
requires a local patch crate or upstream change. It should be tested as its own
runtime experiment, not mixed with socket sharding or `io_uring`.

## RTMP egress chunk-size sweep

The runtime already sends `SetChunkSize` for RTMP egress. A one-variable sweep
tested 4 KiB, 16 KiB, and 64 KiB chunk sizes using RTMP-only MSR loopback runs.

All runs passed MediaMTX path health: every expected path was ready, bytes grew,
and inbound frame errors were zero. Restream and MediaMTX warn/error/panic log
scans were clean for the 16 KiB 300-output and no-env default proof runs.

| Outputs | Chunk | RTMP paths | CPU avg % | CPU peak % | RSS peak | MediaMTX bytes delta |
|---:|---:|---:|---:|---:|---:|---:|
| 120 | 4 KiB | 120/120 | 35.05 | 59.17 | 107 MB | 19.4 MB |
| 120 | 4 KiB | 120/120 | 42.27 | 56.54 | 105 MB | 15.3 MB |
| 120 | 16 KiB | 120/120 | 27.81 | 34.26 | 104 MB | 15.2 MB |
| 120 | 16 KiB | 120/120 | 22.40 | 33.71 | 104 MB | 15.5 MB |
| 120 | 64 KiB | 120/120 | 39.27 | 52.54 | 104 MB | 15.5 MB |
| 300 | 4 KiB | 300/300 | 74.96 | 130.97 | 128 MB | 46.9 MB |
| 300 | 16 KiB | 300/300 | 39.39 | 43.68 | 129 MB | 42.4 MB |
| 120 | default 16 KiB | 120/120 | 25.31 | 33.28 | 105 MB | 13.4 MB |

Conclusion: 16 KiB is the best measured default in these loopback RTMP fanout
runs. It materially reduced sampled restream CPU at 120 and 300 outputs without
increasing RSS meaningfully or weakening receiver proof. 64 KiB helped the
microbench for large frames but did not improve live CPU in the short MSR run.
