# srt-rs upstream work list — from restream integration + MSR scale runs

## Contents

- [P0 — tokio adapter sends one syscall per datagram](#p0--tokio-adapter-sends-one-syscall-per-datagram)
- [P1 — protocol TX allocates + copies per packet, clones per retransmit](#p1--protocol-tx-allocates--copies-per-packet-clones-per-retransmit)
- [P2 — receiver buffer stores full DataPacket per received packet](#p2--receiver-buffer-stores-full-datapacket-per-received-packet)
- [P3 — expose a tokio-native batch receive helper](#p3--expose-a-tokio-native-batch-receive-helper)
- [P4 — shared-listener ACK servicing under many peers](#p4--shared-listener-ack-servicing-under-many-peers)
- [P5 — smaller items noticed during integration](#p5--smaller-items-noticed-during-integration)
- [Non-goals (evaluated and rejected — do not spend time)](#non-goals-evaluated-and-rejected--do-not-spend-time)

Context for the next agent working in `/home/dev/srt-rs`: restream
(github.com/krsna1729/restream) consumes `shiguredo_srt` +
`srt-transport` (tokio feature) pinned at `0821257b` for both ingest and
egress. Full attribution evidence:
`docs/agent-guidance/quality/srt-rs-msr-matrix-2026-08-25.md` on the
restream side. Target workload that exposes every item below: 1,200 SRT
egress leaves × 8 Mb/s each (~456k pps TX), and 1,200-connection ingress,
host loopback, 6 cores.

Items are ordered by measured impact. Each has a concrete reproduction
and a suggested shape; none require wire-format changes.

## P0 — tokio adapter sends one syscall per datagram

`Conn::drain_outputs_bounded` (tokio feature, `crates/srt-transport`
`lib.rs` ~line 6873) does `self.sock.send(&bytes).await` per
`ConnectionOutput::SendPacket`. The **mio** runtime path already batches
via `libc::sendmmsg` with thread-local mmsghdr/iovec scratch
(`send_batch`, ~line 6560) — the tokio path never got the same
treatment.

Measured cost (restream egress, 600 leaves): ~100–120k pps ceiling vs
456k needed; host %sys ≈ 23% while 45% idle; engine log shows 5.4k
feed-overrun resyncs + 4.6k leaf terminations per run as the ACK clock
starves.

Shape: add `send_batch`/`try_send_batch` to the tokio `Conn`
(`UdpSocket::poll_send` loops or `sendmmsg` on the raw fd between
readiness polls), drain up to N packets per poll cycle, keep protocol
ordering on partial send (`prepend_outputs` semantics must be
preserved). Restream will re-run its msr ladder immediately after.

## P1 — protocol TX allocates + copies per packet, clones per retransmit

`srt_connection.rs` ~line 946: `payload.to_vec()` on every app send into
`SenderBuffer`; `srt_sender.rs` lines 370/437/485: `packet.clone()`
(full payload Vec) for each retransmission probe path. At target rate
this is ~600 MB/s memcpy + ~456k allocs/s before any protocol work;
restream's perf profile is dominated by malloc/BTreeMap machinery.

Shape: switch `DataPacket.payload` / `SenderBuffer` storage to
`bytes::Bytes` (already a dependency ecosystem-wide in consumers) or a
caller-supplied buffer pool. The sans-I/O API boundary makes this a
mechanical but wide refactor; keep `feed_recv_buf(&[u8])` signature for
RX.

## P2 — receiver buffer stores full DataPacket per received packet

`ReceiverBuffer.packets: BTreeMap<u32, ReceivedPacket>` where
`ReceivedPacket { packet: DataPacket, recv_time }` and `DataPacket`
owns its payload Vec. Ingress at 8 Mb/s × N connections means another
alloc+copy per packet plus BTreeMap node churn per sequence number.
Consider pooled payload storage keyed by seq range, or an arena per
connection with generation GC after TSBPD release.

## P3 — expose a tokio-native batch receive helper

`recvmsg_batch(fd, ...)` exists as a free fn (good — restream's harness
sink now uses it), but the tokio `Conn::drive`/ingress paths still do
`try_recv_from` loops. Either provide
`Conn::recv_batch(&mut bufs...)` or document the free fn as the blessed
path with an example; restream had to hand-roll fd plumbing to reach it.

## P4 — shared-listener ACK servicing under many peers

`PeerTable::poll_outbound(now, out)` drains *ready* peers once per call;
with >100 peers per socket the inter-visit latency lets inbound ACK
queues build (restream observed 168–243 KB Recv-Q on the egress side's
own listener sockets). A `time_until_next_deadline`-aware hint API
already exists (`time_until_next_deadline`, `DueIndex`) — consider an
advisory "oldest unread socket age" or per-peer RX watermark callback so
applications can prioritize draining sockets whose peers are closest to
flow-window exhaustion.

## P5 — smaller items noticed during integration

1. `ConnectionOptions::default()` sets `max_bandwidth_bytes_per_sec:
   None` = BW_INFINITE (1 Gbps). Live senders then burst-clump until the
   pacing IIR catches up; a documented recommended value or
   `SessionConfig::with_bitrate_hint()` would help applications size
   pacing correctly.
2. `OutputDrainBudget::default()` (64 actions / 32 pkts / 256 KiB) is
   easy to leave hardcoded at call sites (restream did). Consider
   `BudgetClass::{Low,Default,Bulk}` presets so TX-heavy callers don't
   starve silently.
3. `PeerTableConfig::max_peers_per_ip` default equals table max (4096);
   loopback testing and same-datacenter deployments share one source IP,
   so the per-IP bound silently becomes THE bound. Document or make it
   opt-in explicitly.
4. `IngressTelemetry.stranded_conclusions` was invaluable when debugging
   reuseport rehash behavior — similar per-stage counters for the
   egress/caller table (e.g. `budget_exhausted_visits`,
   `backpressured_drains`) would have cut restream's attribution time
   substantially.

## Non-goals (evaluated and rejected — do not spend time)

- Stuffing state into `socket_id` or extending SYN cookies beyond
  worker-index+peer-hash: couples wire identity to topology, widens
  spoof surface, and lookups are not the bottleneck (hash tables are
  FxHash-based and cache-resident; syscalls/copies dominate).
- Changing wire format or admission semantics (StreamID/KM checks are
  load-bearing for restream security posture).
