# SRT pure-Rust migration progress

## Contents

- [Current migration policy](#current-migration-policy)
- [Phase 6: production Rust egress seam](#phase-6-production-rust-egress-seam)
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
output was cleaned up after analysis. The production-shaped `mio` driver was
selected because it had the strongest balanced single-pair CPU/memory result;
the other runtime variants remain useful as later experimental adapters rather
than production defaults.

## Phase 6: production Rust egress seam

The first production-shaped Rust caller path is now wired through the existing
egress fabric without changing the egress engine or protocol engine:

- `SrtLeafHandle` and `SrtConnectedTransport` let a connector own either a
  native libsrt socket or a Rust UDP socket plus its message sender.
- `SrtRustMessageSender` owns a connected nonblocking UDP socket and a Core
  `SrtConnection`; `SrtRustFabricPoller` owns the mio readiness registration.
- `RESTREAM_SRT_BACKEND=libsrt|rust` selects the complete egress connector,
  configurator, and poller at runtime. The default remains `libsrt`.
- The harness sink selector remains explicit: `HARNESS_SRT_SINK_BACKEND=rust`
  or `libsrt`, falling back to `RESTREAM_SRT_BACKEND` when unset. This keeps
  mixed endpoints available for differential testing while preserving the
  final whole-stack deployment shape.

The initial live manual-QA slice used the bench-profile binaries and a
one-output SRT-only MSR run. Both sides passed the output-progress and sink
byte-growth checks with zero sender drops:

| Restream | Sink | Bytes out delta | Restream CPU / RSS peak |
|---|---|---:|---:|
| Rust | Rust | 246,844 | 18.8% / 89,472 KiB |
| Rust | libsrt | 251,356 | 16.6% / 89,636 KiB |
| libsrt | Rust | 248,160 | 38.04% / 90,472 KiB |

These are seam and interop checks, not the scale gate. Bonding, AES-192,
Core timer wakeups during idle periods, pacing-aware send admission, and the
300/700/1200-output differential matrix remain open before Rust egress can be
called production-complete.

The receiver strategy investigation was profiled as paired processes, not
just as restream. Each of the four 600-output Rust-sink captures contains both
`restream.svg` and `sink.svg`, with matching raw perf data and folded stacks:

| Receiver strategy | Restream CPU avg / peak | Restream RSS peak | Sink profile |
|---|---:|---:|---|
| Distinct ports, 4 workers | 219.28% / 231.83% | 637,584 KiB | paired `sink.svg` |
| `SO_REUSEPORT`, 4 workers | 209.03% / 237.18% | 635,536 KiB | paired `sink.svg` |
| Connected handoff, 4 workers | 202.47% / 216.38% | 633,908 KiB | paired `sink.svg` |
| One port per stream, 1 worker | 242.04% / 275.25% | 632,024 KiB | paired `sink.svg` |

The complete profiles and the sink-side interpretation are recorded in
[`srt-scaling-investigation.md`](srt-scaling-investigation.md). The receiver
conclusion is deliberately split from sender conclusions: the restream
flamegraphs share the libsrt `sendmsg`/UDP and mutex/futex cost, while the sink
flamegraphs expose Core receive-buffer, ACK, allocation, and feedback UDP
costs. No topology result is being attributed to only one endpoint.

### Post-commit bench-profile recheck

After commit `90205e72`, `scripts/build/bench-harness.sh` rebuilt both
production-shaped binaries with the verified x86-64-v3 bench profile. The
one-output MSR seam check was repeated in all three endpoint combinations;
these short runs are correctness/interop evidence, not a CPU baseline:

| Restream | Sink | Restream CPU sample | RSS peak | Bytes out delta | Drops |
|---|---|---:|---:|---:|---:|
| Rust | Rust | 7.86% | 89,448 KiB | 251,544 | 0 |
| Rust | libsrt | 8.87% | 89,680 KiB | 246,844 | 0 |
| libsrt | Rust | 24.24% | 90,404 KiB | 247,220 | 0 |

The artifacts were regenerated under `.local/artifacts/msr/` and the run
used `BENCH_BUILD=never`, proving the harness consumed the newly built bench
binaries rather than silently rebuilding debug executables.

### Optimized six-driver smoke

The six swappable Rust drivers and the libsrt control pair were rebuilt with
the same bench profile and exercised at 8 Mbps for three seconds with no
impairment. All seven final cells produced caller/listener stats and exited
successfully; the first compio namespace cell returned `1/1`, but an immediate
exact rerun passed, so it was treated as a transient harness event rather than
a protocol failure.

| Driver | Pair CPU ms/s | Pair RSS KiB | CPU ms/s/Mbps | RSS KiB/Mbps |
|---|---:|---:|---:|---:|
| libsrt | 427.6 | 11,520 | 53.45 | 1,440 |
| mio | 320.7 | 5,120 | 40.09 | 640 |
| tokio | 329.5 | 6,144 | 41.19 | 768 |
| smol | 414.7 | 5,632 | 51.84 | 704 |
| monoio | 277.9 | 5,632 | 34.74 | 704 |
| glommio | 551.8 | 28,160 | 68.98 | 3,520 |
| compio | 439.9 | 5,888 | 54.99 | 736 |

CPU is caller plus listener user/system time normalized by the three-second
run; RSS is the sum of both processes. These are fixed-rate smoke diagnostics,
not a linear scaling law. The paired receiver flamegraphs remain the stronger
topology evidence because they capture both restream and sink under the
600-output workload.

The raw smoke ledgers are
`.local/artifacts/srt-six-driver-smoke-20260817.tsv` and
`.local/artifacts/srt-compio-smoke-20260817.tsv`.

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
