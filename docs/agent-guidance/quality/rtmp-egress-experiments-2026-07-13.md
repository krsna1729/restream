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

## Receiver compatibility and constrained-network evidence

The RTMP chunk-stream specification allows the sender to notify the peer of a
new chunk size and documents the valid range as 128 bytes through 65,536 bytes.
The chosen default, 16 KiB, stays well inside that protocol envelope.

Public platform ingest documentation checked on 2026-07-13 did not expose a
platform-specific RTMP chunk-size limit:

| Platform | Publicly documented ingest constraints found | RTMP chunk-size note |
|---|---|---|
| YouTube Live | RTMP/RTMPS ingest, RTMPS on port 443 with SNI, codec/bitrate/frame-rate/keyframe requirements. | No public chunk-size limit found in YouTube's encoder or RTMPS ingest docs. |
| Facebook Live / Meta Live Video API | RTMP/RTMPS encoder, keyframe cadence, resolution, bitrate, protocol requirements. | No public chunk-size limit found in the Meta Live Video API reference or business help result. |
| Instagram Live Producer | RTMP-based producer workflow with 720x1280, 2.25-6 Mbps, 30 fps preferred settings. | No public chunk-size limit found in Instagram's Live Producer requirements. |
| VdoCipher Live | OBS-style RTMP ingest credentials, 30 fps, 720p/1080p, 2.5-7 Mbps guidance. | No public chunk-size limit found in VdoCipher's public live-streaming guide. |

Sources:

- https://ossrs.net/lts/en-us/assets/files/rtmp_specification_1.0-25a467618b92a3115bc97d4b0038b0ff.pdf
- https://support.google.com/youtube/answer/2853702
- https://developers.google.com/youtube/v3/live/guides/rtmps-ingestion
- https://developers.facebook.com/documentation/live-video-api/reference
- https://www.facebook.com/business/help/162540111070395
- https://about.instagram.com/blog/tips-and-tricks/instagram-live-producer
- https://www.vdocipher.com/blog/flutter-live-streaming-application/

Because there is no second host available for a real NIC path test, the local
substitute was the existing stalled RTMP sink fault mode. That validates the
worst correctness concern for a larger RTMP chunk size: one slow or non-draining
receiver must not stop sibling outputs, and the runtime must surface the back
pressure in output health.

Command:

```sh
RESTREAM_BIN=target/bench/restream \
  WORK_DIR=.local/artifacts/rtmp-chunk16-fault-output-stall-20260713T051850Z \
  scripts/build/resource-limit.sh target/bench/test_harness fault.output-stall --no-netns
```

Result:

| Check | Result |
|---|---|
| Single stalled RTMP sink | PASS; publish accepted, output surfaced `stalled` in `sending` phase. |
| Sibling isolation | PASS; 12 healthy RTMP siblings accepted and continued byte progress while one sibling was stalled. |
| Stalled TCP signal | `tcpNotsentBytes` reached about 16.0 MB, `tcpSndWnd=0`, `tcpSndCwnd=1`, matching a non-draining receiver. |
| Fault log hygiene | One expected `output failed` warning for the injected bad sink; no unexpected panic path. |

With sudo, loopback `tc netem` was also available. A short constrained-link
proof ran 120 RTMP-only MSR outputs with 20 ms added loopback delay and a 500
Mbit/s rate cap:

```sh
sudo tc qdisc replace dev lo root netem delay 20ms rate 500mbit limit 10000

RESTREAM_RTMP_EGRESS_CHUNK_SIZE=16384 \
  MSR_PROTOCOL_MIX=rtmp-only \
  MSR_OUTPUT_COUNTS=120 \
  MSR_SAMPLE_SECS=8 \
  MSR_SAMPLE_INTERVAL_MS=4000 \
  MSR_SINK_SAMPLE_SECS=2 \
  BENCH_BUILD=never \
  WORK_DIR=.local/artifacts/rtmp-chunk16-netem-120-20260713T052108Z \
  scripts/harness/run.sh msr -- --no-netns
```

Result: `PASS`.

| Check | Result |
|---|---:|
| MediaMTX ready | `120/120` |
| MediaMTX bytes delta | `12,382,100` over 2 s |
| Post-ffprobe bytes delta | `13,763,761` over 2 s |
| Inbound frame errors | `0` |
| ffprobe samples | `4/4`, H.264 video plus one selected audio stream |
| CPU avg / peak | `85.38% / 103.17%` |
| RSS peak | `107,708 KiB` |
| Runtime log scan | clean; no warn/error/panic lines |
| qdisc cleanup | restored to `qdisc noqueue` |

Interpretation: this does not replace a second-host NIC test, because loopback
still has loopback MTU and local kernel behavior. It does, however, prove that
the 16 KiB RTMP chunk default survives a constrained TCP path, MediaMTX
continues receiving all 120 streams, and sampled outputs remain probeable. CPU
is much higher than the unconstrained 120-output 16 KiB loopback runs, which is
expected when the sender spends more time in the shaped TCP path; that is not a
regression in the chunk-size change itself.

The remaining external caveat is real network validation through a second host
or controlled external RTMP endpoint to cover physical MTU, NIC queues, WAN
loss, and receiver implementation variance. TCP should segment 16 KiB
application writes normally, and the netem run supports that path locally.
