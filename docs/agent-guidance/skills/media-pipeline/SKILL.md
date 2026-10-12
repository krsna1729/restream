---
name: media-pipeline
description: Use before changing restream src/media/, RTMP/SRT/HLS protocol paths, transcoder stages, ring buffers, mux/demux, egress fabrics, or any per-packet hot path.
---

# Media Pipeline

Read before changing `src/media/`:

- [architecture](../../../architecture.md)
- [media pipeline](../../../media-pipeline.md)
- [high-performance data path](../../../high-performance-data-path.md)
- [testing](../../../testing.md)

## Ownership

- Tokio owns the control plane: API handlers, reconciliation, timers and
  application session state. Per-packet media work does not belong on
  Tokio: SRT and RTMP ingest media run to completion on their ingress
  owners; the shared TS mux and the HLS segmenter are still on Tokio and are
  moving to the owner that publishes the feed.
- SRT transport sockets and protocol state (`PeerTable`, timers, ACK/NAK, TX)
  are owned by dedicated Compio `Owner` threads: one ingress owner thread per
  listener member (`RESTREAM_SRT_INGRESS_OWNERS`, default 1; more than one
  share the port as a `SO_REUSEPORT` group and pass srt-rs
  `ListenerTransfer`s between their threads, never through Tokio), and one
  Compio runtime with at most one `Owner` per address family per egress
  shard. Never move SRT sockets or `PeerTable` state onto Tokio; address SRT
  ingress sessions from Tokio only by `IngressPeer` (Owner index +
  `LogicalPeerId`) through bounded commands and events.
- RTMP/RTMPS sockets stay on the native TCP/io_uring shard workers until the
  Compio TCP migration.
- Blocking FFmpeg and other blocking calls belong on dedicated OS threads or
  the blocking pool, never on Tokio workers or an Owner thread.
- Wrap OS-thread entry points that cross native FFmpeg/FFI boundaries with
  `catch_unwind(AssertUnwindSafe(...))`; the Compio Owner threads report an
  `Owner` fault as a bridge event, and a panic there closes the bridge, which
  Tokio treats as a listener stop.
- No internal or external failure path may crash the engine; isolate faults
  and surface errors.

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

Hot paths include `src/media/`, ring buffers, mux/demux loops, AVIO queues,
SRT/RTMP packet loops, HLS segmenting, and transcoder data paths.

- Benchmark before and after with the relevant `benches/` suite.
- Avoid per-packet allocation, logging, serialization, locks, async channel
  sends, and system calls.
- Do not add logging inside packet-level loops in
  `src/media/ring_buffer.rs` or `src/media/avio.rs`.
- Use burst APIs where available.
- Hoist reusable buffers outside loops and clear them inside the loop.
- Prefer `Bytes` and `BytesMut` ownership transfer over payload copies.
- Do not add diagnostic readers or metrics that alter production pipeline
  behavior.
- Keep protocol correctness tests at least as strong as performance
  validation.
- For SIMD, benchmark scalar first, keep a scalar fallback, use runtime
  feature detection, and minimize `unsafe`.

## Verification

- Timestamp/DTS/PTS logic: `cargo test <new test name>`.
- Protocol behavior: one harness mode for the new scenario; see
  [protocol-test](../protocol-test/SKILL.md).
- Lifecycle, thread hops or teardown: see
  [concurrency-proof](../concurrency-proof/SKILL.md).
- Hot-path code: see [perf-sweep](../perf-sweep/SKILL.md).
