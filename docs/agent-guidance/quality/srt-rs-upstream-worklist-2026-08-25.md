# srt-rs upstream work list — from restream integration + MSR scale runs

## Contents

- [P0 — Tokio shared-socket send batching (primitive merged; integration open)](#p0--tokio-shared-socket-send-batching-primitive-merged-integration-open)
- [P1 — protocol TX allocates + copies per packet, clones per retransmit](#p1--protocol-tx-allocates--copies-per-packet-clones-per-retransmit)
- [P2 — receiver buffer stores full DataPacket per received packet](#p2--receiver-buffer-stores-full-datapacket-per-received-packet)
- [P3 — expose a tokio-native batch receive helper](#p3--expose-a-tokio-native-batch-receive-helper)
- [P4 — shared-listener ACK servicing under many peers](#p4--shared-listener-ack-servicing-under-many-peers)
- [P5 — smaller items noticed during integration](#p5--smaller-items-noticed-during-integration)
- [2026-08-29 exact-pin status](#2026-08-29-exact-pin-status)
- [Non-goals (evaluated and rejected — do not spend time)](#non-goals-evaluated-and-rejected--do-not-spend-time)

Context for the next agent working in `https://github.com/krsna1729/srt-rs`:
restream consumed the old pin
`0821257b402b08219aaaf38a62f5fa655a7e4947` and now cuts over to the exact
pin `901a912778e8b4fee6c8c122a6dec963282a8e8a` for both ingest and egress.
Full attribution evidence:
`docs/agent-guidance/quality/srt-rs-msr-matrix-2026-08-25.md` on the
restream side. Target workload that exposes every item below: 1,200 SRT
egress leaves × 8 Mb/s each (~456k pps TX), and 1,200-connection ingress,
host loopback, 6 cores.

Items are ordered by measured impact. Each has a concrete reproduction
and a suggested shape; none require wire-format changes.

## 2026-08-29 exact-pin status

Current measured pin:
`901a912778e8b4fee6c8c122a6dec963282a8e8a`.

- Transport plumbing, including `LogicalCallerMut::send_shared`, merged in
  [PR 24](https://github.com/krsna1729/srt-rs/pull/24) and is consumed by both
  Restream and the standalone live relay.
- Shared-caller pacing remains open as
  [#20](https://github.com/krsna1729/srt-rs/issues/20).
- The cumulative ACK scan tracked in
  [#25](https://github.com/krsna1729/srt-rs/issues/25) is fixed by merged
  [PR 27](https://github.com/krsna1729/srt-rs/pull/27). Its former 37.76% self
  cost disappears from the `c347f11` profile.
- Tokio shared-socket `sendmsg_batch` merged in
  [PR 28](https://github.com/krsna1729/srt-rs/pull/28), closing
  [#26](https://github.com/krsna1729/srt-rs/issues/26). Restream and the
  standalone relay use the primitive with partial-send preservation and Tokio
  readiness integration. The upstream benchmark helper still clears partial
  batches and bypasses readiness bookkeeping; tracked as
  [#29](https://github.com/krsna1729/srt-rs/issues/29).
- The standalone 30-output cell passes at 7.991 Mb/s worst receiver with relay
  CPU down to 37.2%. Across three 200-output repetitions, every connection
  remains active and median relay ingress reaches 7.514 Mb/s, but median worst
  receiver is only 6.141 Mb/s. The receiver hot path is now tracked as
  [#30](https://github.com/krsna1729/srt-rs/issues/30). See
  `tools/srt-fanout-bench/results/2026-08-29-901a912.md`.

## P0 — Tokio shared-socket send batching (primitive merged; integration open)

`sendmsg_batch` merged in PR 28 and Restream now uses it for shard-owned
shared-socket egress. Per-connection Tokio `Conn::drain_outputs_bounded`
still sends one datagram at a time; that is acceptable for one socket/peer and
is not the dense fan-out topology.

Measured cost (restream egress, 600 leaves): ~100–120k pps ceiling vs
456k needed; host %sys ≈ 23% while 45% idle; engine log shows 5.4k
feed-overrun resyncs + 4.6k leaf terminations per run as the ACK clock
starves.

Remaining integration work is tracked as issue 29: invoke raw batch syscalls
through Tokio readiness, preserve the unsent suffix on partial `sendmmsg`,
bound receive bursts, and propagate errors. Restream's adapter implements
those send-side invariants and the standalone 30-output relay CPU falls from
49.2% to 37.2%.

## P1 — protocol TX allocates + copies per packet, clones per retransmit

The merged `send_shared(Bytes)` path shares retained payload ownership across
callers. `SenderBuffer::push_shared_with_sequence` still calls
`payload.to_vec()` for each caller's immediate `DataPacket`, and packet
encoding allocates another destination-specific wire buffer. At target rate
this remains hundreds of thousands of allocations/copies per second. Tracked
with live copy-rate math as issue 31.

Shape: switch `DataPacket.payload` / `SenderBuffer` storage to
`bytes::Bytes` (already a dependency ecosystem-wide in consumers) or a
caller-supplied buffer pool. The sans-I/O API boundary makes this a
mechanical but wide refactor; keep `feed_recv_buf(&[u8])` signature for
RX.

## P2 — receiver buffer stores full DataPacket per received packet

`ReceiverBuffer` now stores a stripped `ReceivedPacket`, but still converts
payload `Vec` to `Box<[u8]>`, performs BTreeMap insert/remove per packet, and
converts back to `Vec` for `DataReceived`. At 200-way ingress,
`ReceiverBuffer::pop_ready` is 17.69% self and `receive` is 9.20% self. The
live receiver ceiling and suggested batch/ownership work are tracked as issue
30.

## P3 — expose a tokio-native batch receive helper

`recvmsg_batch(fd, ...)` exists as a free fn, but the Tokio `Conn::drive` and
application ingress paths still need a readiness-safe wrapper. Calling the raw
fd helper directly after `UdpSocket::readable()` can leave Tokio readiness
latched; the first standalone integration starved timers/control work. Either provide
`Conn::recv_batch(&mut bufs...)` or document the free fn as the blessed
path with a `try_io` example and a bounded-burst contract.

## P4 — shared-listener ACK servicing under many peers

`PeerTable::poll_outbound(now, out)` drains *ready* peers once per call;
with >100 peers per socket the inter-visit latency lets inbound ACK
queues build (restream observed 168–243 KB Recv-Q on the egress side's
own listener sockets). A `time_until_next_deadline`-aware hint API
already exists (`time_until_next_deadline`, `DueIndex`) — consider an
advisory "oldest unread socket age" or per-peer RX watermark callback so
applications can prioritize draining sockets whose peers are closest to
flow-window exhaustion.

Cutover note: the new pin brings the protocol-side TSBPD/pacing/ACK work
forward enough to keep this item scoped to application/runtime servicing,
not wire semantics. Preserve CTR/default behavior. Input-bandwidth pacing
remains a separate opt-in, measured follow-up rather than part of the
default cutover.

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

Also preserve the current cutover boundaries in upstream follow-up notes:
bonded shared-sequence handling lands with the cutover, KM rejection and
crypto hardening stay mandatory, optional AES-GCM is additive only, and the
remaining bottlenecks are still tokio per-datagram sends plus protocol-side
`Vec`/copy churn.

## Non-goals (evaluated and rejected — do not spend time)

- Stuffing state into `socket_id` or extending SYN cookies beyond
  worker-index+peer-hash: couples wire identity to topology, widens
  spoof surface, and lookups are not the bottleneck (hash tables are
  FxHash-based and cache-resident; syscalls/copies dominate).
- Changing wire format or admission semantics (StreamID/KM checks are
  load-bearing for restream security posture).
- Treating AES-GCM or input-bandwidth pacing as implicit default changes for
  this cutover. Both are separate opt-in follow-ups and need their own
  measurement work; CTR/default behavior stays preserved in the cutover.
