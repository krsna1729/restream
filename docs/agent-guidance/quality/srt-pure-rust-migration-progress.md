# SRT pure-Rust migration progress

## Contents

- [Current migration policy](#current-migration-policy)
- [Phase 6: production Rust egress seam](#phase-6-production-rust-egress-seam)
- [Phase 4: TLPKTDROP receiver accounting](#phase-4-tlpktdrop-receiver-accounting)
- [Affinity invariant for tuple sharding and bonding](#affinity-invariant-for-tuple-sharding-and-bonding)
- [Paired Rust egress timer/wakeup profile — 2026-08-18](#paired-rust-egress-timerwakeup-profile--2026-08-18)
- [Production Rust publish-ingest seam — 2026-08-18](#production-rust-publish-ingest-seam--2026-08-18)
- [Production GROUP handshake metadata — 2026-08-18](#production-group-handshake-metadata--2026-08-18)
- [Core Broadcast/Backup group machine — 2026-08-18](#core-broadcastbackup-group-machine--2026-08-18)
- [Rust sink GROUP admission — 2026-08-18](#rust-sink-group-admission--2026-08-18)

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

These are seam and interop checks, not the scale gate. Bonding, Core timer
wakeups during idle periods, pacing-aware send admission, and the
300/700/1200-output differential matrix remain open before Rust egress can be
called production-complete.

### AES-192 parity and retained three-way interop evidence

The local libsrt reference accepts 'SRTO_PBKEYLEN' values 16, 24, and 32 and
advertises AES-192 as handshake encryption field 3. The Rust Core previously
implemented only AES-128 and AES-256, and both Rust adapter boundaries
rejected pbkeylen=24. The parity fix adds AES-192 to AES-CTR, AES-KW,
handshake-field mapping, the production Rust caller, and the Rust harness
sink.

The first live attempt failed before any bytes were produced because the MSR
output URL stayed plaintext while the Rust sink was configured for AES-192.
The native harness sink also initially did not apply its crypto settings to
the listener. Both harness boundaries now consume the same HarnessSrtCrypto
tuple, so the differential cases exercise encryption rather than merely the
plaintext seam:

| Restream | Sink | Result | Bytes out delta | Drops | Restream CPU sample | RSS peak |
|---|---|---|---:|---:|---:|---:|
| Rust | Rust | PASS | 251,356 | 0 | 26.62% | 89,400 KiB |
| Rust | libsrt | PASS | 246,468 | 0 | 13.85% | 90,268 KiB |
| libsrt | Rust | PASS | 249,852 | 0 | 23.81% | 90,564 KiB |

All runs used MSR_OUTPUT_COUNTS=1, MSR_PROTOCOL_MIX=srt-only,
HARNESS_SRT_PBKEYLEN=24, and BENCH_BUILD=never after
scripts/build/bench-harness.sh. The retained reports are:

- .local/artifacts/srt-aes192-rust-rust/msr-results.json
- .local/artifacts/srt-aes192-rust-libsrt/msr-results.json
- .local/artifacts/srt-aes192-libsrt-rust/msr-results.json

This closes AES-192 parity for the tested caller/listener combinations; it
does not close bonding, idle timer, pacing, ingest, or scale gates.

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

### High-impairment recheck and smol readiness fix

The first 10% loss / 100ms one-way delay bake-off exposed two measurement
fairness problems rather than a Rust protocol limit. Every interop binary had
a hard-coded five-second handshake deadline, which was too short for this
impaired path. The deadline is now a shared 15-second constant, so all seven
pairs get the same opportunity to establish the connection.

The smol pair still failed intermittently after that fairness change. A direct
run showed the listener reaching `CONNECTED` while the caller reported
`handshake timeout`. The smol adapter had waited for readiness and then called
the raw socket; that bypassed async-io's optimistic read plus retry sequence.
The adapter now uses async-io's `recv`/`recv_from` read path for the blocking
branch and retains a raw nonblocking drain for queued datagrams. Five repeated
smol cells passed after the change, and the full high-impairment matrix passed
all seven pairs:

| Pair | Caller/listener | Caller packets | Listener packets | Caller retransmits | Listener loss events |
|---|---:|---:|---:|---:|---:|
| libsrt | 0 / 0 | 7,612 total | 6,691 total | 769 | 683 |
| mio | 0 / 0 | 7,587 | 7,415 | 1,512 | 773 |
| tokio | 0 / 0 | 7,588 | 7,415 | 1,713 | 815 |
| smol | 0 / 0 | 7,587 | 7,418 | 1,591 | 917 |
| monoio | 0 / 0 | 7,586 | 7,416 | 1,659 | 781 |
| glommio | 0 / 0 | 7,586 | 7,404 | 1,625 | 810 |
| compio | 0 / 0 | 7,586 | 7,413 | 1,681 | 884 |

The exact raw output is
`.local/artifacts/srt-six-driver-loss10-delay100-10s-readfix-20260817.tsv`.
The matrix was run from the optimized bench-profile binaries with
`target-cpu=x86-64-v3`; it is a protocol/differential gate, not a claim that
the runtimes have equal CPU cost. The four receiver-topology flamegraph runs
remain separately paired: every strategy directory contains both
`restream.svg` and `sink.svg`, so sender and receiver pathologies are not
being inferred from one endpoint's profile.

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

## Paired Rust egress timer/wakeup profile — 2026-08-18

The Rust production egress path now exposes transport timer and pacing
deadlines to the egress shard. Idle handshake, retransmit, ACK, keepalive, and
inactivity timers wake the shard without requiring a readable UDP event.
Pacing-blocked application queues suppress writable polling until their send
deadline, avoiding a permanently writable-UDP scheduler loop. A deterministic
regression test covers timer service without fd readiness:
`idle_transport_deadline_is_serviced_without_fd_readiness`.

The first paired Rust/Rust 600-output run deliberately used one sink port. It
stalled at 561/600 and the restream log recorded repeated `handshake timeout`
events to the same sink port (`127.0.0.1:31642`). This is receiver admission
pressure, not a protocol interop failure: the same Rust sender and receiver
passed 30/30 and 120/120, and the measured 8-port/4-worker receiver topology
passed 600/600.

The valid profile used `PEER_COUNT=8`,
`HARNESS_SRT_SINK_SCALING=ports`, `HARNESS_SRT_SINK_THREADS=4`, Rust egress,
Rust sink, SRT-only MSR, and 600 outputs. `perf record -F 99 -g` captured the
restream process and all sink worker threads concurrently. Both flamegraphs,
raw perf data, folded stacks, reports, and the result JSON are retained in
`.local/artifacts/msr-rust-egress-ports8-profile-20260818/`.

| Endpoint | CPU / RSS evidence | Result |
|---|---:|---:|
| Restream | 249.37% average, 257.41% peak; 188,940 KiB RSS | Rust egress, 600/600 |
| Rust sink | profiled across 11 sink/runtime TIDs | 8 ports / 4 workers |
| Differential gate | 165,937,448 bytes growth; 0 sender drops | PASS |

The restream flamegraph shows the expected Rust UDP/SRT path: packet send in
`SrtRustMessageSender::flush_outputs`, receive/feedback in `service`, Core
`feed_recv_buf`, ACK generation, and timer handling. The new global wakeup
scan is visible as `SrtShardBackend::next_wakeup` and
`SrtRustMessageSender::next_timer_deadline`; it is measurable but small enough
to be the next optimization target rather than a correctness blocker.

The sink flamegraph is receiver-side evidence, not a restream proxy. Its
largest named Rust costs are `SrtConnection::feed_recv_buf` (1.59%),
`ReceiverBuffer::pop_ready` (1.28%), `_int_malloc` (0.81%),
`process_rust_connections_mode` (0.80%), `ReceiverBuffer::generate_ack`
(0.66%), and `ReceiverBuffer::receive` (0.62%), with `epoll_wait` accounting
for the expected idle/waiting share. There is no dominant worker lock convoy.
The next receiver work is therefore allocation/receive-buffer/ACK reduction,
while the next restream work is replacing the per-idle global deadline scan
with a cached or heap-backed wakeup index.

## Production Rust publish-ingest seam — 2026-08-18

The first production restream receiver seam is now live behind
`RESTREAM_SRT_BACKEND=rust`. It binds one non-bonded SRT UDP listener per
configured Rust ingest worker with `SO_REUSEPORT`; Linux keeps each sender UDP
tuple on one worker, and the worker owns the corresponding sans-I/O
`SrtConnection` state. A bounded worker-to-Tokio event channel preserves the
existing StreamID parsing, ban check, pipeline authentication, MPEG-TS demux,
and `forward_ingest_packets` media boundary. Clean Core disconnects are now
recorded as disconnects rather than ingest errors.

Static gates passed after the seam was split into the listener coordinator,
packet pump, connection service, and media session modules:

- `cargo fmt --all --check`
- `scripts/build/resource-limit.sh cargo test rust_ingest --lib` — the worker
  budget contract passed
- `scripts/build/resource-limit.sh cargo clippy --lib -- -D warnings`

The optimized live binaries were rebuilt by `scripts/build/bench-harness.sh`.
Its x86-64-v3 feature preflight passed on this host, and it applied
`-C target-cpu=x86-64-v3` to the bench profile. The Rust/Rust live differential
slice used `mixed.live.srt.h264.a1.bf0` with `MSR_PEER=sink`,
`HARNESS_SRT_SINK_BACKEND=rust`, and `RESTREAM_SRT_BACKEND=rust`. It passed
16/16 outputs, the Rust sink probe (`69` video packets, `7` audio packets, `3`
keyframes, monotone DTS), HLS preview/upload, recording, stage-sharing, and
lifecycle checks. The retained artifact is
`.local/artifacts/mixed/live/srt/h264/a1/bf0/`; its restream log records
`Rust SRT ingest listener started` with two workers.

This is intentionally a development seam, not the final whole-stack claim.
Rust ingest currently admits non-bonded `publish` only; SRT `read` and bonding
remain unavailable in this mode. Because the Core listener must select crypto
and TSBPD delay before the handshake completes, the first seam accepts only
pipelines whose resolved crypto and latency equal the global listener policy.
Per-pipeline handshake policy, Rust Broadcast bonding, and Rust Backup
failover are the next production seams. The current tuple map is deliberate:
wire data packets carry the destination socket ID, not a reliable caller
source socket ID, so later logical connections from an identical UDP tuple
 cannot be safely split by socket ID alone.

## Production GROUP handshake metadata — 2026-08-18

The first bonding increment adds the libsrt-compatible group wire metadata to
the production `shiguredo_srt` Core. The local libsrt reference was used as
the authority: `srtcore/core.cpp::fillHsExtGroup` writes two network-order
32-bit words under command `SRT_CMD_GROUP` (8), and both
`interpretGroup` and `runAcceptHook` scan it only when the ordinary CONFIG
extension flag is set. The packed second word is `[type:8][flags:8][weight:16]`;
the group ID must carry `SRTGROUP_MASK` (bit 30).

The production implementation now provides `GroupType`, `GroupExtensionData`,
`SRTGROUP_MASK`, `GFLAG_SYNCONMSG`, `HandshakePacket::add_group_extension`,
and `HandshakePacket::get_group_extension`. `ConnectionOptions` can carry
local group metadata, `SrtConnection` emits it on both conclusion legs, and
stores the peer's group metadata for the future group worker. The exploratory
`/home/dev/srt-rs` implementation was not copied: its separate GROUP flag
would be incompatible with the pinned libsrt parser, which uses CONFIG.

Proof completed:

- The red test failed before the production group types and methods existed.
- The green handshake test validates the exact eight bytes for a Broadcast
  group (`40 00 12 34 01 00 00 c8`) and the CONFIG bit.
- The sans-I/O connection test establishes caller and listener with distinct
  group IDs and verifies both peer metadata directions.
- `scripts/build/resource-limit.sh cargo test -p shiguredo_srt --lib`
  passed (104 tests), the focused connection test passed, and
  `scripts/build/resource-limit.sh cargo check --workspace` passed.

This increment is wire/identity groundwork only. It does not claim bonded
routing: group membership, shared sequence ownership, Broadcast merge/dedup,
disconnect removal, Backup promotion, and paired restream/sink profiles
remain the next gates.

## Core Broadcast/Backup group machine — 2026-08-18

The next increment makes the group behavior explicit in the protocol Core with
`SrtGroup`. Its receive path follows the libsrt reference's shared bond receive
model (`srtcore/group.cpp::recv`): data from every leg is collected into one
sequence-keyed pending set, the lowest eligible sequence is delivered once, and
all member receivers advance past that sequence. This makes duplicate
Broadcast legs and a packet arriving on a different leg observable in the same
way instead of letting each connection deliver independently.

The send path now has both modes:

- Broadcast sends one coordinated sequence number to every active member.
- Backup selects the highest-weight active/standby member, promotes a standby
  after failure, aligns the promoted sender to the group's next sequence, and
  continues without reusing a sequence.

The connection and sender seams needed for this are explicit sequence-bearing
`DataReceived` events, coordinated send sequence injection, and receiver
advancement after a group delivery. A red/green regression test caught the
important promotion case: an unused standby still had sequence zero when the
group had already sent sequence zero on the primary. The fix synchronizes an
empty promoted sender to the group sequence before sending; the focused group
suite now passes all three Broadcast/Backup tests.

Proof completed:

- `cargo test -p shiguredo_srt --test test_srt_group` passed (3 tests).
- The existing Core library and connection tests remain green.

This is still a Core seam, not a production bonding claim. The harness and
restream adapters must next admit multiple GROUP legs into one group worker,
remove disconnected members, and be exercised in paired restream/sink live
tests for Rust↔Rust and libsrt interop. Those profiles must retain the earlier
receiver-strategy rule: capture both the restream process and the sink worker.

## Rust sink GROUP admission — 2026-08-18

The Rust harness sink now has a receiver-side GROUP admission path for its
distinct-port, one-port-per-stream, and SO_REUSEPORT loops. It decodes GROUP
and StreamID only from a conclusion handshake, keys one logical group by the
peer group ID plus normalized StreamID, allocates one listener-side mirror
group ID, and routes every leg to the same `SrtGroup`. Each leg keeps its own
tuple and timers; the group Core performs the shared sequence merge/dedup and
removes broken members. This follows the libsrt `makeMePeerOf` behavior rather
than treating StreamID or socket ID as a per-datagram sharding key.

Proof completed:

- The admission unit test proves that repeated legs reuse one mirror ID and
  preserve the caller's link weight.
- The ignored live test
  `rust_sink_accepts_libsrt_broadcast_and_backup_groups` passed against the
  pinned native `restream-srt-bond-client` for both Broadcast and Backup;
  Backup includes closing the weighted primary and sending again.
- The regular harness sink tests and `cargo check --bin test_harness` pass.

Scope boundary: the Connected listener-to-connected-datagram handoff still
uses its non-bonded tuple worker map, so bonded traffic through that strategy
must be routed through the same group registry before it is production-ready.
No Rust restream egress group driver exists yet, and no paired restream/sink
bonding profile is claimed by this increment. The paired profile contract
remains mandatory for the next scale evidence: profile the restream process
and the named Rust sink worker(s) in the same run.
