---
name: media-pipeline
description: Use before changing restream src/media/, crates/restream-*/src/, RTMP/SRT/HLS protocol paths, transcoder stages, ring buffers, mux/demux, egress fabrics, per-packet hot paths, or thread/runtime placement.
---

# Media Pipeline

Read before changing `src/media/` or `crates/restream-*/src/`:

- [architecture](../../../architecture.md)
- [media pipeline](../../../media-pipeline.md)
- [high-performance data path](../../../high-performance-data-path.md)
- [testing](../../../testing.md)

## Ownership

- Tokio CONTROL owns the control plane: API handlers, reconciliation, timers
  and application session state. Per-packet media work never runs there:
  RTMP, SRT and MoQ ingest media run to completion on their Compio owners;
  shared TS muxing, audio routers, HLS segmenting and recording run on the
  MEDIA executor (`restream-media`); egress runs on `egress-shard-N`.
- SRT transport sockets and protocol state (`PeerTable`, timers, ACK/NAK, TX)
  are owned by dedicated Compio `Owner` threads: one ingress owner thread per
  listener member (`RESTREAM_SRT_INGRESS_OWNERS`, default 1; more than one
  share the port as a `SO_REUSEPORT` group and pass srt-rs
  `ListenerTransfer`s between their threads, never through Tokio), and one
  Compio runtime with at most one `Owner` per address family per egress
  shard. Never move SRT sockets or `PeerTable` state onto Tokio; address SRT
  ingress sessions from Tokio only by `IngressPeer` (Owner index +
  `LogicalPeerId`) through bounded commands and events.
- RTMP/RTMPS sockets stay on their Compio owner: ingress owners for ingest,
  egress shards for output.
- Blocking calls (child pipes, files) belong on dedicated OS threads or the
  blocking pool, never on Tokio workers or an Owner thread. Codecs run only in
  FFmpeg child processes.
- Wrap OS-thread entry points with `catch_unwind(AssertUnwindSafe(...))`; the Compio Owner threads report an
  `Owner` fault as a bridge event, and a panic there closes the bridge, which
  Tokio treats as a listener stop.

## Media semantics

- Keep media timestamps separate from wall-clock/application time.
- Respect `MediaPacket.format`; consumers must handle `Flv` and `Raw`
  explicitly.
- RTMP video timestamps are DTS; signed FLV composition offset derives PTS.
- Normalize SRT Stream IDs before lookup.
- Duplicate SRT publishers are not bonded ingest; bonding is an `srt-rs`
  group connection (one `LogicalPeerId`), never independent publishers
  sharing a StreamID.
- HLS storage is in-memory unless an explicit design change says otherwise.

## Hot paths

Hot paths include `src/media/`, `crates/restream-*/src/`, ring buffers, mux/demux
loops, the recording byte queue, SRT/RTMP packet loops, HLS segmenting, and transcoder data paths.

- Follow the hot-path contract in
  [high-performance data path](../../../high-performance-data-path.md#hot-path-contracts):
  no per-packet allocation, logging, serialization, locks, async channel
  sends or syscalls; burst APIs, hoisted buffers and `Bytes` ownership
  transfer instead of copies.
- Benchmark before and after with the relevant `benches/` suite.
- For SIMD, benchmark scalar first, keep a scalar fallback, use runtime
  feature detection, and minimize `unsafe`.

## Verification

- Timestamp/DTS/PTS logic: `cargo test <new test name>`.
- Protocol behavior: one harness mode for the new scenario; see
  [protocol-test](../protocol-test/SKILL.md).
- Lifecycle, thread hops or teardown: see
  [concurrency-proof](../concurrency-proof/SKILL.md).
- Hot-path code: see [perf-sweep](../perf-sweep/SKILL.md).
