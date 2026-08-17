# SRT pure-Rust migration progress

## Contents

- [Current migration policy](#current-migration-policy)
- [Phase 4: TLPKTDROP receiver accounting](#phase-4-tlpktdrop-receiver-accounting)
- [Affinity invariant for tuple sharding and bonding](#affinity-invariant-for-tuple-sharding-and-bonding)

## Current migration policy

The final deployment shape is process-wide and all-or-nothing:

- `libsrt` mode uses the complete native SRT stack, including the harness SRT
  sink/listener for control runs.
- `rust` mode uses the complete Rust SRT stack, including the harness SRT
  sink/listener and both Broadcast and Backup bonding.
- Mixed Rust/libsrt endpoints are allowed only during Phases 6-8 for
  differential testing and to isolate egress, ingest, and bonding failures.
  They are not a final deployment policy.

The exploratory implementation at `/home/dev/srt-rs/src/srt_group.rs` is an
input to the Rust bonding work. It is not copied wholesale: its group API and
example-level socket loops must be audited against the pinned libsrt wire
behavior before the production Core/Driver path adopts any part of it.

The MSR sink boundary now has the same explicit split. `MSR_PEER=sink` uses
the native pool by default; `HARNESS_SRT_SINK_BACKEND=rust` binds a dedicated
pure-Rust `SrtConnection` receiver pool driven by one mio readiness loop over
all sink ports. If the harness-specific variable is absent, it follows
`RESTREAM_SRT_BACKEND`. Rust egress measurements therefore have a Rust sink
option and do not rely on a libsrt receiver.

## Phase 4: TLPKTDROP receiver accounting

Fixed the receiver-head regression in `crates/srt-protocol/src/srt_receiver.rs`.
When TLPKTDROP permanently removes a missing sequence, `drop_too_late()` now
advances `expected_seq` across the dropped sequence and any contiguous buffered
packets. This matches libsrt's `CRcvBuffer::dropUpTo()` behavior and prevents
`receive()` from rediscovering the same permanent hole on every later packet.

Evidence:

- The regression test fails without the fix (`expected_seq`: `1000`, expected
  `1002`) and passes with it.
- `scripts/build/resource-limit.sh cargo test -p shiguredo_srt` passes: 101
  unit tests, 1 allocation-guard test, 2 buffer tests, 4 crypto tests, 4 error
  tests, 27 connection tests, and 1 doctest.
- The exact 10% loss / 100ms delay / 8Mbps / 60s mio run reports 23,026
  packets received and 2,347 loss events, instead of the pre-fix runaway
  counter.

The corrected driver-framework bake-off was completed and its temporary raw
output was cleaned up after analysis. The current recommendation is to build
the production-shaped `mio` driver first: it had the strongest balanced
single-pair CPU/memory result, while the other runtime variants remain useful
as later experimental adapters rather than production defaults.

## Affinity invariant for tuple sharding and bonding

Tuple affinity is necessary for correctness, but it is not sufficient for a
bonded receiver:

- Non-bonded traffic uses the full UDP 4-tuple as the socket-owner key. A
  connected worker owns that tuple for its lifetime; a shared tuple cannot be
  split by `SO_REUSEPORT` or by a connected handoff.
- Bonded traffic first uses the SRT handshake's `GROUP` ID and type to select
  one logical bond worker. Normalized StreamID validates the application
  identity. Each leg then keeps its own tuple and socket ID for transport
  state. Socket ID alone cannot join legs because every physical leg has its
  own ID, and StreamID alone cannot prove bonding.
- GROUP and StreamID are handshake metadata, not per-datagram keys. The
  receiver must cache them and route later datagrams through the selected
  group/tuple state. Least-load selection applies only when creating a new
  tuple or bond; disconnects must remove the corresponding active membership.

This matches the stock libsrt reference: `SRT_CMD_GROUP` carries group ID,
type, flags, and weight; `makeMePeerOf` finds or creates the peer group by
group identity and rejects type collisions. The exploratory Rust code mirrors
the same GROUP extension, but production bonding still needs this affinity
table and shared group receive/merge state.

The fourth sink topology is explicit as
`HARNESS_SRT_SINK_SCALING=per-stream-port`: the SRT-only MSR harness binds one
Rust UDP port per output slot. It skips unused RTMP sink listeners and rejects
overlap with restream's own SRT port; the live 1,200-port result is recorded in
the scaling investigation artifact.

That result is now complete: one sink worker owned all 1,200 ports, the full
MSR SRT-only checkpoint reached 1,200/1,200 outputs with zero sender drops,
restream peaked at 275.43% CPU and 1,276,580 KiB RSS. A direct profile of the
sink worker showed receive-buffer, ACK, allocation, and kernel receive/send
costs rather than a lock convoy. The paired restream profile showed Tokio and
libsrt epoll/futex waiting. The run required `ulimit -n 65535`; the default
1,024 limit failed at the 1,016th socket.

The latest tuple-cardinality check showed why this distinction matters:
600 independent source tuples balanced connected handoff at 150 tuples per
worker, yet still produced 34 sender-side drops at 371.0% CPU; high-tuple
`SO_REUSEPORT` passed the 600-output checkpoint but produced 254,558 drops at
342.7% CPU. At the full 1,200-output target, that same high-tuple
`SO_REUSEPORT` setup timed out before its first checkpoint at 180 seconds.
Tuple distribution fixes ownership skew, not the SRT protocol or per-session
cost.
