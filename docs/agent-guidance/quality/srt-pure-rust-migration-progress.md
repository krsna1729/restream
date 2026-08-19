# SRT pure-Rust migration progress

## Contents

- [Current migration policy](#current-migration-policy)
- [Phase 6: production Rust egress seam](#phase-6-production-rust-egress-seam)
- [Phase 4: TLPKTDROP receiver accounting](#phase-4-tlpktdrop-receiver-accounting)
- [Affinity invariant for tuple sharding and bonding](#affinity-invariant-for-tuple-sharding-and-bonding)
- [Paired Rust egress timer/wakeup profile — 2026-08-18](#paired-rust-egress-timerwakeup-profile--2026-08-18)
- [Production Rust publish-ingest seam — 2026-08-18](#production-rust-publish-ingest-seam--2026-08-18)
- [Production Rust read/play seam — 2026-08-18](#production-rust-readplay-seam--2026-08-18)
- [Rust receive-buffer parity seam — 2026-08-18](#rust-receive-buffer-parity-seam--2026-08-18)
- [Production GROUP handshake metadata — 2026-08-18](#production-group-handshake-metadata--2026-08-18)
- [Core Broadcast/Backup group machine — 2026-08-18](#core-broadcastbackup-group-machine--2026-08-18)
- [Rust sink GROUP admission — 2026-08-18](#rust-sink-group-admission--2026-08-18)
- [Production Rust bonded egress and paired endpoint profile — 2026-08-18](#production-rust-bonded-egress-and-paired-endpoint-profile--2026-08-18)
- [Explicit production Broadcast/Backup mode matrix — 2026-08-18](#explicit-production-broadcastbackup-mode-matrix--2026-08-18)
- [Socket-ID bonded feedback routing and live Backup failover — 2026-08-19](#socket-id-bonded-feedback-routing-and-live-backup-failover--2026-08-19)
- [Backup receive-batch ordering correction — 2026-08-18](#backup-receivebatch-ordering-correction--2026-08-18)
- [Kernel symbols and connected affinity profile — 2026-08-18](#kernel-symbols-and-connected-affinity-profile--2026-08-18)
- [Kernel-symbol and profiling toolchain verification — 2026-08-18](#kernel-symbol-and-profiling-toolchain-verification--2026-08-18)
- [Reusable SRT lifecycle crate extraction — 2026-08-18](#reusable-srt-lifecycle-crate-extraction--2026-08-18)
- [Six runtime adapter contract and compio ownership — 2026-08-18](#six-runtime-adapter-contract-and-compio-ownership--2026-08-18)
- [Interop crate layering audit — 2026-08-19](#interop-crate-layering-audit--2026-08-19)
- [Receiver bookkeeping optimization and current scale boundary — 2026-08-19](#receiver-bookkeeping-optimization-and-current-scale-boundary--2026-08-19)

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

The crate-layer decision is now explicit. Keep `crates/srt-protocol` as the
sans-I/O wire/protocol Core. Extract the duplicated handshake admission,
packet-key aliasing, GROUP plus normalized StreamID affinity, connected
handoff, and teardown policy into a new runtime-neutral `crates/srt-lifecycle`
crate. Restream and the harness keep their Mio/Tokio/other-framework socket
adapters; no framework is pulled into the lifecycle crate and no generic event
loop is imposed on future users. The six `srt-interop` framework modules remain
benchmark adapters and interop executables, not the reusable lifecycle API.

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
`HARNESS_SRT_SINK_BACKEND=rust`, and `RESTREAM_SRT_BACKEND=rust`. The exact
optimized-source recheck passed 16/16 outputs, the Rust sink probe (`80` video
packets, `8` audio packets, `3` keyframes, monotone DTS), HLS preview/upload,
recording, stage-sharing, pipeline deletion, and zero-residue checks. The
retained artifact is
`.local/artifacts/mixed/live/srt/h264/a1/bf0/`; its restream log records
`Rust SRT ingest listener started` with two workers.

This is intentionally a development seam, not the final whole-stack claim.
Rust ingest admits non-bonded `publish` and the connected receiver now admits
libsrt Broadcast and Backup GROUP handshakes; SRT `read` and production media
failover remain separate gates. The listener now decodes the incoming
StreamID from the conclusion datagram, resolves the shared per-stream policy,
and applies crypto plus TSBPD delay before the Core processes KMREQ. This
matches the libsrt accept-hook ordering. The Rust Core now also consumes the
resolved per-stream FC and internal receive-buffer packet capacity before
conclusion, using libsrt's exact 1472-byte default-MSS conversion for the
12 MiB RCVBUF. Kernel UDP receive-buffer sizing still uses the global startup
setting and remains separate from this protocol-buffer parity. The
current tuple map is deliberate: wire data packets carry the destination
socket ID, not a reliable caller source socket ID, so later logical connections
from an identical UDP tuple cannot safely be split by socket ID alone.

## Production Rust read/play seam — 2026-08-18

Rust SRT `read` admission now uses the same listener-side StreamID policy and
active-pipeline check as the native play path. The read task owns the media
reader and sends bounded `WorkerCommand` messages back to the worker that owns
the accepted UDP tuple; disconnect cancellation removes both the media reader
and the worker session.

The follow-up review found and fixed three lifecycle details before handoff:
the worker authorization command is now queued before the read task starts,
so an early `Send` cannot overtake ownership; the per-connection cancellation
token now participates in the media wait, so an idle active pipeline wakes and
drops its `TsChunkReader` on disconnect; and both the reuse-port and connected
worker wait calculations include the Core's next pacing deadline whenever
fragmented output is queued. The latter prevents a valid reader from being
rounded down to one packet per 20 ms poll interval. Production-path tests now
exercise the actual fragment sender, idle-session cancellation, outbound byte
limits, and a connected Core pacing deadline.

The first live read attempt exposed a real Rust-only boundary: a muxed video
burst reached `SrtConnection::send` as one `734,704`-byte message. The Core's
single-packet send path emitted that as one UDP datagram, so the caller received
no media. The read seam now fragments every batch to the existing 1,316-byte
MPEG-TS/SRT payload size before crossing the worker boundary. A second trace
then exposed the independent outbound queue guard: 256 entries was smaller
than one valid fragmented burst. The outbound guard is now 4,096 entries with
the existing 4 MiB byte cap; the inbound authorization queue remains 256
entries.

Evidence from the optimized, x86-64-v3 bench binaries built from the current
source:

- `srt.policy` with `RESTREAM_SRT_BACKEND=rust` and
  `HARNESS_SRT_SINK_BACKEND=rust` passed all five cases, including inherited
  encrypted-16 policy, per-pipeline plaintext under an encrypted global,
  encrypted-24 and encrypted-32 pipelines, and the plaintext/wrong-passphrase
  rejection probes. The retained result is
  `.local/artifacts/srt.policy.rust/results.json`.
- The identical `RESTREAM_SRT_BACKEND=libsrt` /
  `HARNESS_SRT_SINK_BACKEND=libsrt` control passed the same five cases. Its
  retained result is `.local/artifacts/srt.policy.libsrt/results.json`.
- `mixed.live.srt.h264.a1.bf0` with the Rust ingest and Rust sink passed the
  burst graph, Rust sink media probe, pipeline deletion, and zero-residue
  checks. Its retained scenario is
  `.local/artifacts/mixed/live/srt/h264/a1/bf0/scenario.json`.
- The focused Rust read unit test verifies that payloads are split at the
  1,316-byte boundary by observing `WorkerCommand::Send` messages from the
  production helper; the read cancellation, queue-limit, and Core pacing
  regression tests also pass. The existing Core bidirectional send test passes.

The first production run exposed the remaining stale application-layer guard:
the Core admission fix was correct, but `authenticate_connection` still
rejected policies whose crypto or latency differed from the global startup
configuration. Removing that guard made the Rust and libsrt matrices agree.
The receive-buffer parity seam is now closed. The remaining limitation is
intentional and documented: the caller can propose a higher PEERLATENCY than
the local pre-conclusion policy-sized capacity, and the kernel UDP queue is
still global. The next evidence-led phase is the broader loss/latency/bitrate/
scaling performance loop across restream, the Rust sink, and all six runtime
adapters.

The implementation commit passed `cargo test --lib srt::` (279 tests),
`cargo clippy --lib -- -D warnings`, `cargo fmt --all --check`, the Markdown
check, the concurrency fast gate, and `scripts/build/bench-harness.sh`. The
bench build verified host support for `x86-64-v3` and used the bench profile's
`opt-level=3`, thin LTO, and `-C target-cpu=x86-64-v3` settings. No `restream`,
`mediamtx`, or `ffmpeg` processes remained after either live run.

The layering decision remains evidence-led. `srt-protocol` owns the sans-I/O
wire state machine, `srt-lifecycle` owns reusable admission/affinity/GROUP
policy, and restream owns its media/auth/read task and Mio worker adapter.
The six `srt-interop` binaries currently exercise direct protocol/runtime
adapters and have no admission, GROUP lifecycle, or worker handoff path to
reuse. Adding `srt-lifecycle` as a dependency now would be cosmetic and would
not make their standalone measurements represent the production read seam.
When lifecycle-backed multi-connection benchmarks are added, they should
depend on `srt-lifecycle` explicitly and measure that policy path.

## Rust receive-buffer parity seam — 2026-08-18

The source audit found that the Rust Core had two independent fixed 8,192
packet limits: `ReceiverBuffer::max_buffer_size` and every handshake's flow
window. Pinned libsrt instead allocates `CRcvBuffer` from `SRTO_RCVBUF`,
converts bytes to packets with `MSS - UDP header` (1,472 bytes at the default
MSS), and caps the result by `SRTO_FC`. At the restream default of 12 MiB and
FC 32,768, the native effective capacity is 8,548 packets.

The fix keeps the dependency direction intact. `buffer_sizing` remains the
restream policy owner and now exposes the pure resolution needed by both
backends. `ConnectionOptions` carries the already-resolved flow window and
receive capacity; `set_listener_policy` installs them before conclusion;
`SrtConnection` advertises and creates the receiver with the effective
`min(RCVBUF packets, FC)` capacity. No policy lookup, allocation, or
extra branch was added to the datagram receive hot path. The Rust harness sink
uses the same 12 MiB/FC preset as its libsrt sink, and Rust single/group
egress consumes the resolved FC and RCVBUF packet capacity rather than falling
back to 8,192.

Evidence:

- The red protocol test failed because listener policy accepted only crypto
  and latency; it now passes while checking both the full ACK capacity and
  the conclusion's advertised flow window.
- `ingest_buffer_packets_match_libsrt_byte_conversion` passes with the exact
  8,548-packet default conversion.
- `egress_buffer_packets_match_libsrt_byte_conversion` passes with the exact
  712-packet conversion for the 1 MiB egress RCVBUF, including the FC cap.
- `cargo test -p shiguredo_srt --lib` passes 106 tests, the protocol
  integration suite passes 30 tests, and the Rust ingest focused suite passes
  12 tests.
- `cargo check --bin test_harness` and `cargo fmt --all -- --check` pass after
  updating the harness sink and Rust egress callers.
- A fresh optimized Rust/Rust policy run and identical optimized libsrt control
  each pass all five cases after correcting the advertised effective flight
  capacity; both leave no media processes behind.

This closes the internal protocol-buffer parity item, not the kernel queue
item: Rust listener UDP `SO_RCVBUF` remains a global per-socket startup value.
It also cannot pre-size for a caller's higher `PEERLATENCY`, because that
value is learned only after the pre-conclusion policy window. The next loop
must therefore measure high-latency loss/latency/bitrate behavior and then
resume the six-runtime and receiver-strategy profiles with both protocol and
kernel counters recorded.

### Buffer formula allocation audit — 2026-08-18

The formula is safe but conservative for the current MSR target. At 8 Mbps
and 250 ms, the raw latency window is 250 KB; the existing 4x safety margin
needs 1 MB. The resulting comparison is:

| Path | Current default at 250 ms | 8 Mbps, 4x model | Oversizing |
|---|---:|---:|---:|
| Rust/native ingest floor | 12 MiB / 8,548 packets | 1 MB / 679 packets | 12.6x |
| Native egress, unknown bitrate | 6.25 MB / ~4,245 packets | 1 MB | 6.25x |
| Native egress, known 8 Mbps | 2 MB floor / ~1,358 packets | 1 MB | 2x |

At 1,200 streams those configured ceilings correspond to 14.4 GB, 7.5 GB,
and 2.4 GB respectively. These are upper bounds, not proof that all pages
are resident: libsrt grows and occupies its per-socket userspace buffers with
traffic and retransmission state, while the Rust receiver's ordered map does
not pre-reserve the full packet ceiling.

The existing measurements show the cost is real. The 1,200-output native
SRT-only run measured 3,990,917 KB RSS, about 3.33 MB per output, while its
socket log reported `SRT snd=6102KB`; the configured 6.25 MB ceiling was not
fully resident but still dominated the per-output memory cost. The earlier
320-output attribution measured 493 MB of unattributed private dirty memory,
and the severe 20% loss / 150 ms ± 40 ms jitter test grew only 12–28 KB per
connection above its baseline. The latter shows the current ceiling is not
being approached by the tested loss profile.

Therefore the allocation is not optimal for a known 8 Mbps/250 ms workload,
although the 4x model itself is a reasonable starting safety margin. Do not
lower the production default solely from arithmetic: the next performance
loop must A/B the current policy against a bitrate-aware 1 MB model and a
2 MB safety floor at 30/300/700/1200 streams, recording drops, latency,
RSS/PSS, native `msSndBuf`, and CPU/flamegraph changes. A reduction is valid
only if all four scales retain zero drops under the loss/jitter scenarios.

### Controlled sink-buffer A/B — 2026-08-18

The harness now accepts the same test-only policy for both sink backends:
`HARNESS_SRT_SINK_FC_PACKETS` and `HARNESS_SRT_SINK_RCVBUF_BYTES`. The native
sink applies the values as `SRTO_FC`/`SRTO_SNDBUF`/`SRTO_RCVBUF`; the Rust sink
converts the byte value using libsrt's 1,472-byte packet conversion and caps
the result by FC. This makes the measurement a policy comparison rather than
an advantage for either receiver.

The first valid whole-stack A/B used the optimized `target/bench` binaries,
300 SRT outputs, one sink port, and one sink worker. Every case reached 300/300
outputs with zero sender drops:

| Stack | Sink policy | Restream CPU avg | RSS peak | PSS peak | RSS change vs 1 MB |
|---|---:|---:|---:|---:|---:|
| Rust/Rust | 1,000,000 bytes / 679 packets | 170.82% | 127,704 KiB | 122,421 KiB | baseline |
| Rust/Rust | 2 MiB / 1,424 packets | 148.69% | 131,468 KiB | 124,459 KiB | +3,764 KiB |
| libsrt/libsrt | 1,000,000 bytes | 65.47% | 304,628 KiB | 297,690 KiB | baseline |
| libsrt/libsrt | 2 MiB | 94.51% | 323,836 KiB | 316,613 KiB | +19,208 KiB |

The RSS delta from 1 MB to 2 MiB was about 12.5 KiB/output for Rust and
64.0 KiB/output for libsrt in this n=300 run. This is evidence that the
configured ceiling is not eagerly allocated one-for-one: neither backend
showed a 1 MB-per-output RSS jump. It is also evidence that libsrt carries a
larger per-connection native footprint on this workload. The CPU difference is
not a buffer-size conclusion: it is a whole-stack implementation difference,
and the one-run CPU values are not a substitute for a repeated perf profile.

The attempted 1,200-output reduction runs did not produce a valid buffer-only
comparison. With one shared Rust listener, the 1 MB run fell from 903 to 2
progressing outputs over 174 seconds. With the established 8-port/4-worker
mitigation it fell from 1,173 to 196 over 215 seconds. In the one-port-per-
stream topology, both the 1 MB run and a same-topology 12 MiB control produced
Rust inactivity timeouts before the output checkpoint. Those artifacts show a
receiver worker/socket-servicing limit, not a formula result, and are retained
under `.local/artifacts/msr-rust-sink-buffer-ab-20260818-common/`,
`.local/artifacts/msr-rust-sink-buffer-ab-20260818-8p4t/`, and
`.local/artifacts/msr-rust-sink-buffer-ab-20260818-per-stream/`.

Current conclusion: the 12 MiB ingest floor is conservative, and the 4x
bitrate-times-latency model is not wasteful as a safety rule, but a 1 MB
receiver policy is not yet admissible for the 1,200 target. The remaining
blocker is sink scaling and connection servicing; it must be fixed and
profiled before a smaller policy can be evaluated at target scale. The 300
case supports measuring a 2 MiB candidate next, not changing the production
default yet. The reproducible 300-output reports are in
`.local/artifacts/msr-rust-sink-buffer-ab-20260818-common/`.

### Bonding assignment lifecycle audit — 2026-08-18

The handshake contains enough information for correct bonding, but not at the
first packet boundary. A listener normally receives INDUCTION first; native
libsrt does not include GROUP there. GROUP and StreamID become available when
the listener processes CONCLUSION, immediately before the Core emits
`ConnectionEvent::Connected`. The peer socket ID is also cached at that point
and identifies the individual physical leg, not the logical bond.

This makes the handoff timing decisive. The harness connected sink creates its
worker-owned connected UDP socket on the first datagram. That is correct for
locking one UDP tuple to one worker and passed the tested loopback bond, where
both legs shared a source IP and provisional source-IP affinity kept them
together. It is not a universal bonded assignment rule: legs arriving from
different source IPs can be split before GROUP is known, and a connected socket
cannot later be moved between workers without moving the Core state and any
packets already buffered during the transition.

Production Rust ingest now has that listener-owned admission path when
`RESTREAM_SRT_INGEST_SCALING=connected` is selected. The listener assigns a
provisional tuple owner, parses GROUP on INDUCTION or CONCLUSION, installs a
local mirror GROUP extension before feeding the GROUP-bearing packet, and
hands off the complete Core state before publishing `Connected`. The worker
creates and registers the connected datagram socket, then emits `Connected`
from the worker-owned state; this makes the subsequent authorization command
causally follow the handoff rather than racing it on the Tokio channel. Data
queued behind the handshake remains in the Core event queue until admission.
The worker then moves each bonded leg into one
`(peer_group_id, normalized_stream_id)` group owner containing each leg's tuple
and socket-ID-specific transport state.

The first native production attempt failed before CONCLUSION because the Rust
listener did not echo a GROUP extension; libsrt logged `the listener did not
respond with group ID` and rejected both Broadcast and Backup. After adding
the mirror response, the pinned native caller reached `connected_group` for
both types, and the production log emitted one logical publisher connection
per two-leg group rather than one session per leg. Evidence is retained under
`.local/artifacts/connected-production-bond-qa-20260818c/`. This distinguishes
the actual handshake defect from connected-socket handoff: the handoff was
not reached until the GROUP response was corrected.

A second lifecycle audit found an independent ordering race in the first
handoff implementation: publishing `Connected` from the listener before its
`Handoff` command was enqueued allowed Tokio to send `Authorize` first. The
worker could then discard authorization because it did not yet own the peer.
The listener now transfers the Core first, and the worker emits `Connected`
only after creating/registering the connected socket. `Authorize` therefore
cannot precede ownership, while handshake and post-handshake data remain in
the same Core event queue.

The final optimized-binary recheck is retained under
`.local/artifacts/connected-production-bond-qa-20260818g/`: the native
Broadcast/Backup group test passed with Backup failover, and the connected
MPEG-TS smoke passed 16/16 output checks.

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
and StreamID from any handshake that carries the extensions, keys one logical
group by the peer group ID plus normalized StreamID, allocates one listener-side mirror
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

The Connected listener-to-connected-datagram handoff now preserves the same
tuple owner, but feeds the worker through the shared GROUP admission and merge
state. Connected group timers and group membership are included in worker
liveness, so a bonded peer is not released while its group legs are active.
Native libsrt sends its first induction datagram without GROUP metadata. The
listener therefore uses source-IP rendezvous as a provisional owner, then
caches the observed GROUP key. This is a correctness guard against splitting
bond legs before admission, not a final scale policy: independent streams from
the same source IP currently contend on that one connected worker. The trace
reports both `source_worker_reuses` and per-worker tuple assignments so this
tradeoff remains visible in every profile.
The Rust restream egress group driver and paired endpoint profiles are recorded
below. The paired profile contract remains mandatory for every later scale
comparison: profile the restream process and the named Rust sink worker(s) in
the same run.

## Production Rust bonded egress and paired endpoint profile — 2026-08-18

The production Rust egress connector now selects a group sender whenever the
resolved SRT URL has two or more peer addresses. `SrtRustGroupSender` owns one
nonblocking UDP socket and multiplexes one Core `SrtConnection` per bond leg
behind the same Rust poller FD. It uses the same Backup group type and primary
weight ordering as the current libsrt egress path; the single-peer path remains
separate and unchanged. This preserves the final whole-stack policy while
allowing mixed endpoint interop during validation.

The MSR harness has a test-only `MSR_SRT_BOND=1` switch. It adds a second leg
to the same sink endpoint, which is the relevant one-public-port case for
testing socket-ID-based logical grouping without changing ordinary MSR URLs.
The first whole-stack Rust/Rust bonded smoke passed at 30 outputs with
`HARNESS_SRT_SINK_SCALING=reuseport`, four sink workers, zero sender drops, and
7,567,564 bytes of sink-side byte growth. Differential 30-output bonded smokes
also passed in both mixed directions: Rust egress to the native libsrt sink
produced 7,469,240 bytes with zero drops, and native libsrt egress to the Rust
sink produced 8,049,596 bytes with zero drops.

The Connected receiver path was then exercised with
`HARNESS_SRT_SINK_CONNECTED_ROUTING=least-tuples` and four workers. It passed
30/30 outputs with zero drops and 30 listener handoffs. The trace observed 30
tuple assignments and three socket IDs per tuple: the two bonded SRT legs plus
the listener-side connection identity, all remaining under one connected worker
owner.

The paired profile used the optimized `target/bench` binaries, 120 bonded SRT
outputs, Rust restream ingest and egress, Rust sink, one public sink port with
four `SO_REUSEPORT` workers, and a 30-second resource window. It captured the
restream process and all sink-process threads in the same live interval:

| Endpoint | Resource result | Profile evidence |
|---|---:|---|
| Restream | 137.33% average / 188.52% peak CPU; 116,768 KiB RSS peak | 431 samples; 0 lost samples |
| Rust sink | four `harness-srt-rust-sink-*` workers | 328 samples; 0 lost samples |
| Differential gate | 120/120 outputs; 26,518,152 bytes growth; 0 sender drops | PASS |

Artifacts are retained under
`.local/artifacts/msr-rust-bond-profile-20260818/`, including `restream.svg`,
`sink.svg`, raw perf data, folded stacks, symbol reports, and the MSR result
JSON/CSV. The restream flamegraph's largest repeated path is kernel UDP receive
queue wakeup work below `UdpSocket::send_to` and
`SrtRustGroupSender::flush_outputs` (6.26% on one egress shard and 5.10% on
another in the symbol report). `next_timer_deadline` is visible at 0.93% in
the report; group sequence/state helpers are small. This identifies per-leg
UDP syscall and loopback wakeup amplification as the current egress limit,
not a group lock or sequence-merge bottleneck.

The sink profile shows the same feedback traffic from the opposite direction:
kernel wakeup/send paths account for the largest samples, followed by the
`run_rust_sink_pool` loop, `receive_rust_packets` (2.44%), allocation (`malloc`
2.13%), hash calculation (1.83%), Core `SrtConnection::feed_recv_buf`
(1.22%), and `SrtGroup::poll_data` (0.61%). `epoll_wait` is expected idle
time, and no dominant worker lock convoy appears. Receiver-side work is now
most plausibly packet feedback plus allocation/hash and receive-buffer cost;
any batching or allocation reuse must be evaluated as a cross-strategy
optimization after the four receiver ownership strategies have each been
profiled at both endpoints.

The Connected GROUP implementation has its own paired 120-output profile under
`.local/artifacts/msr-rust-bond-connected-profile-20260818/`. It reached
120/120 outputs, 26,444,832 bytes of sink-side growth, and zero sender drops;
the restream resource window measured 118.06% average / 171.79% peak CPU and
114,896 KiB RSS peak. The paired perf captures contain 1K restream samples and
2K sink samples, with zero lost samples. The connected restream flamegraph
again centers on `SrtRustGroupSender::flush_outputs` and per-leg
`UdpSocket::send_to` (roughly 7.3-9.0% per egress shard), while the sink
flamegraph attributes the largest work to connected-worker epoll and feedback
`sendto`/UDP paths (38.81% syscall children, 17.99% sendto, 10.36%
`epoll_wait`), followed by `drain_rust_outputs_mode` (19.84% inclusive), Core
`feed_recv_buf` (2.05%), allocation (1.66%), and `SrtGroup::poll_data` (0.64%).
This is consistent with the other receiver strategies: connected handoff adds
one listener/worker forwarding stage, but does not remove the per-datagram
protocol syscall or feedback cost.

The kernel samples resolve because profiling used the pinned Linux perf binary
with `sudo`; kernel address-map restriction remains noted in the generated
reports. The profile is suitable for syscall/wakeup attribution and not for
claiming fully symbolized kernel internals.

The remaining bonding gates are Broadcast and Backup scale profiles at
300/700/1200 outputs and the final whole-stack Rust/Rust production soak.
Rust-egress to native libsrt group-receiver interop, native libsrt egress to
Rust GROUP admission, Connected-mode GROUP admission, and the basic live Rust
Backup failover path are now live-verified. The four receiver-strategy
comparison must continue to treat sink cost and restream cost as separate
columns.

## Explicit production Broadcast/Backup mode matrix — 2026-08-18

The production egress mode is now explicit and typed end to end. The URL
parser accepts `bondmode=broadcast|backup`; omission and unknown values retain
the historical Backup default. Native egress maps the mode to
`SRT_GTYPE_BROADCAST` (`1`) or `SRT_GTYPE_BACKUP` (`2`). Rust egress maps it to
the matching `GroupMode` and handshake `GroupType`. The MSR sink harness uses
`MSR_SRT_BOND_MODE` to exercise the same URL path. This keeps the two backend
modes whole-stack selectable while allowing mixed backends for differential
testing.

The optimized `target/bench` binaries were used for all eight 30-output runs.
Each run used two bonded legs, `MSR_PROTOCOL_MIX=srt-only`, four Rust sink
workers where applicable, and the sink verification window. Every matrix cell
passed with 30/30 outputs and zero sink-reported packet drops:

| Egress | Sink | Mode | CPU avg (%) | RSS peak (KiB) | Bytes out | Result |
|---|---|---|---:|---:|---:|---|
| Rust | Rust | Broadcast | 48.67 | 91,036 | 7,856,144 | PASS, 30/30, 0 drops |
| Rust | Rust | Backup | 53.60 | 90,840 | 7,521,504 | PASS, 30/30, 0 drops |
| Rust | libsrt | Broadcast | 40.66 | 90,452 | 7,505,148 | PASS, 30/30, 0 drops |
| Rust | libsrt | Backup | 37.72 | 90,172 | 7,461,720 | PASS, 30/30, 0 drops |
| libsrt | Rust | Broadcast | 119.99 | 141,700 | 7,618,136 | PASS, 30/30, 0 drops |
| libsrt | Rust | Backup | 78.68 | 141,880 | 7,671,716 | PASS, 30/30, 0 drops |
| libsrt | libsrt | Broadcast | 93.85 | 143,388 | 7,511,164 | PASS, 30/30, 0 drops |
| libsrt | libsrt | Backup | 103.12 | 143,484 | 7,488,040 | PASS, 30/30, 0 drops |

The artifacts are retained under
`.local/artifacts/srt-bond-mode-20260818-<egress>-<sink>-<mode>-30/`.
The CPU/RSS values are run-level MSR resource-window measurements, not
isolated sink-worker measurements; the paired endpoint perf/profile work
remains the authority for hot-path attribution.

The native reference failover helper was also rerun against the pinned
libsrt build. Broadcast accepted two members and delivered one message;
Backup accepted two members, closed the weight-1 primary, promoted the
standby, and delivered the post-close message (`failover=1`). Both exited
zero. Logs are retained under
`.local/artifacts/srt-bond-mode-20260818-native-failover/`. This proves the
libsrt reference failover path. The repeated live Rust production failover
result is recorded below.

## Socket-ID bonded feedback routing and live Backup failover — 2026-08-19

The Rust bonded sender stored SRT group member IDs as one-based values in its
socket-ID map. Its feedback receive path then added one more before indexing
the Core group, so feedback from two legs sharing one peer tuple could be
delivered to the wrong member. This was a member-identity boundary error, not
a timer or group-promotion failure.

The regression test
`same_peer_feedback_reaches_the_socket_id_member` constructs two real SRT
connection pairs with the same peer tuple, sends feedback from the second
pair, and routes it through `SrtRustGroupSender::receive_packets`. It failed
before the correction and passed after the receive path used the already
one-based member ID directly. The existing unknown-socket-ID tuple fallback
remains covered separately.

The production-shaped manual QA used optimized x86-64-v3 bench binaries,
Rust restream egress, a two-leg `bondmode=backup` URL, and the pinned native
GROUP receiver. The native receiver deliberately closed the weight-1 primary
after the first message. Two independent runs passed:

| Run | Receiver evidence | Restream/harness evidence |
|---|---|---|
| `.local/artifacts/msr-rust-bond-failover-20260819/` | `closed_primary_after_first=1`; `members_before=2`; `members_after=1`; `messages=2`; `bytes=2632` | `outputs=1/1`; `bytesOutDelta=112424`; `outputsPresent=1`; `status=PASS` |
| `.local/artifacts/msr-rust-bond-failover-20260819-retry/` | `closed_primary_after_first=1`; `members_before=2`; `members_after=1`; `messages=2`; `bytes=2632` | `outputs=1/1`; `bytesOutDelta=101144`; `outputsPresent=1`; `status=PASS` |

The receiver logged `GROUP RCV-DROPPED` packets at the deliberate primary
close. Those are queued packets abandoned with the closed primary; they are
not hidden as a clean zero-loss claim. The failover contract itself is
verified by the post-close second message, clean process exit, and successful
restream output checkpoint. This closes basic Rust Backup failover, but not
the sustained gap/duplicate budget or the 300/700/1200 scale gates.

## Backup receive-batch ordering correction — 2026-08-18

The Core receive path had a concrete Backup race that was not covered by the
previous promotion test. `SrtGroup::poll_data` refreshed a member's
`ConnectionState::Disconnected` before draining its queued events. If the
active member's final DATA and SHUTDOWN and the standby's first post-promotion
DATA were observed in one driver batch, the active DATA was rejected before its
SHUTDOWN event was consumed, and the standby DATA was evaluated while the
member was still `Standby`.

The fix in `crates/srt-protocol/src/srt_group.rs` keeps pending admission state
until event draining, evaluates Backup data admission per event, promotes the
standby immediately after consuming the active disconnect, and performs the
full disconnected-state refresh afterward. The regression test
`backup_delivers_standby_payload_arriving_with_active_shutdown` feeds that
ordering with real `SrtConnection` packets and verifies ordered `primary` then
`backup` delivery plus member promotion.

Evidence:

- The test failed before the fix with `backup` as the first delivered payload.
- `cargo test -p shiguredo_srt` passed with 106 unit tests, the allocation guard,
  2 buffer tests, 5 crypto tests, 4 error tests, 30 connection tests, 4 group
  tests, and the doctest.
- `cargo clippy -p shiguredo_srt --all-targets -- -D warnings` passed.
- The ignored native interop test ran the real Rust sink against the pinned
  native client for both Broadcast and Backup and passed.

This closes one receive-batch correctness seam. Combined with the repeated
live result above, deliberately killed-member behavior is now covered at the
basic production seam; sustained gap/duplicate measurement and the
300/700/1200 Broadcast/Backup scale profiles remain required.

## Kernel-symbol and profiling toolchain verification — 2026-08-18

The host profiling toolchain is now installed and verified for the running
`6.8.0-137-generic` kernel: the matching `linux-tools`/`perf`, headers,
`dwarves`/`pahole`, `bpftrace`, `trace-cmd`, KernelShark, Hotspot, elfutils,
LLVM, and Cargo Flamegraph 0.6.14. `kernel.perf_event_paranoid=-1` and
`kernel.kptr_restrict=0` are active for this boot.

The host exposes the matching BTF image at `/sys/kernel/btf/vmlinux` and
`/boot/System.map-6.8.0-137-generic`. The exact Noble ddeb packages
`linux-image-6.8.0-137-generic-dbgsym` and
`linux-image-unsigned-6.8.0-137-generic-dbgsym` are not published, so older
kernel debug packages were not substituted. Current captures are suitable for
kernel function/syscall/softirq attribution through BTF, kallsyms, and
`System.map`; kernel source-line attribution requires a matching 137 DWARF
image from the kernel provider.

## Reusable SRT lifecycle crate extraction — 2026-08-18

The lifecycle seam is now implemented as `crates/srt-lifecycle`, rather than
remaining a plan-only boundary. It depends only on `crates/srt-protocol` and
standard library collections. Its public policy surface is the generic
transport-key `WorkerRouter`, `GroupAffinity`, `LogicalGroupKey`,
`RoutingMode`, StreamID normalization, and handshake GROUP extraction. It does
not depend on Mio, Tokio, any of the six benchmark runtimes, threads, sockets,
media, authorization, or wall-clock time.

Restream connected ingest now uses `WorkerRouter<SocketAddr>` and the shared
GROUP/StreamID handshake route. The harness connected sink uses the same
policy with its own `RustSinkConnectionKey` transport key, retaining the
protocol socket-ID dimension locally. This keeps runtime-specific socket
creation and connected-datagram ownership in each adapter while eliminating
the duplicated affinity, disconnect cleanup, and handshake parsing policy.

Verification after extraction:

- `cargo test -p srt-lifecycle`: 3 passed, 0 failed.
- `cargo test --bin test_harness harness_srt_sink`: 10 passed, 1 native-only
  test ignored, 0 failed.
- `cargo clippy -p srt-lifecycle --all-targets -- -D warnings` and the
  harness all-target clippy gate passed.
- Optimized Rust/Rust bonded MSR smoke reached 30/30 outputs with 0 sender
  drops and 0 live-process residue after the run. This is an integration
  correctness check, not a performance comparison; performance tuning resumes
  only after the remaining SRT integration seams consume the same layering.

## Six runtime adapter contract and compio ownership — 2026-08-18

The six framework implementations remain deliberately separate runtime
adapters in `crates/srt-interop`: mio, Tokio, smol, monoio, glommio, and
compio. They all consume the same `srt-protocol` Core and expose the same
external caller/listener arguments and STATS contract, but they do not share a
runtime trait. This preserves the meaningful differences between readiness,
task, and completion models while keeping the dependency direction one-way:

```text
srt-protocol -> srt-lifecycle -> application adapter
                    ^
                    +-- runtime-specific socket/task ownership
                        (mio, tokio, smol, monoio, glommio, compio)
```

This is integration evidence, not a performance result. The canonical
`scripts/build/srt-interop-bench.sh` build verified the host's x86-64-v3
features and produced optimized binaries with opt-level 3, thin LTO, and
`target-cpu=x86-64-v3`. The bounded live smoke in
`.local/artifacts/srt-seven-framework-interop-fixed-20260818.tsv` used those
binaries plus the native libsrt control at 1 Mbps, 50 ms SRT latency, 0%
loss, 0 ms netem delay, and three seconds per cell. All seven pairs exited
0, emitted caller and listener STATS, and reported zero retransmits and zero
receiver loss.

The first smoke exposed an adapter-only defect that compile checks could not
see. Compio's pacing loop canceled a single-shot `recv`/`recv_from` whenever
its timer won. Three optimized 10-second zero-loss repeats reproduced 43--49
caller retransmits and 35--42 receiver loss events. The compio runtime submits
owned-buffer operations and cancellation can discard a completion after the
kernel has consumed a datagram. That behavior does not belong in
`srt-protocol` or `srt-lifecycle`.

The compio adapter now has a dedicated receive task that owns one receive
operation at a time and forwards completed datagrams to the pacing loop; the
pacing loop never cancels a receive. Three post-fix optimized repeats under
`.local/artifacts/srt-compio-zero-loss-fixed-20260818-*.tsv` all exited 0 with
zero retransmits and zero receiver loss. An intermediate multishot-stream
attempt was rejected because it delivered no packets and grew RSS to roughly
5 GiB; it was discarded rather than hidden behind a passing exit code.

The layering conclusion is therefore evidence-backed: do not add a generic
runtime event-loop crate. Keep shared lifecycle policy in `srt-lifecycle`,
keep socket and receive-operation ownership in each runtime adapter, and only
promote a helper when two adapters demonstrate the same policy without
erasing runtime-specific ownership semantics. Broader loss/latency/bitrate
and scale measurements remain later gates after the integrated adapters and
bonding paths are complete.

## Interop crate layering audit — 2026-08-19

The standalone `crates/srt-interop/Cargo.toml` was audited against the
extracted lifecycle boundary. Its only local SRT dependency is
`crates/srt-protocol`; the six driver modules contain caller/listener socket
ownership and the external STATS contract, but no `WorkerRouter`,
`GroupAffinity`, GROUP/StreamID admission, logical-group teardown, or
connected-worker handoff. The direct source search found no lifecycle policy
duplicated in this crate.

This makes the protocol-only dependency intentional rather than an incomplete
extraction: `srt-interop` is a standalone transport/driver bake-off, while
restream and the harness are the application adapters that need
`srt-lifecycle`. Adding the lifecycle crate to `srt-interop` without a caller
would add dependency surface and falsely imply that the benchmark binaries
exercise production routing policy. The six-driver benchmark remains layered
as `srt-protocol` plus runtime-specific adapters; production routing remains
`srt-protocol` plus `srt-lifecycle` plus an application adapter.

## Receiver bookkeeping optimization and current scale boundary — 2026-08-19

The receiver hot path now has two evidence-backed fast paths in
`crates/srt-protocol/src/srt_receiver.rs`. It caches the circularly oldest
buffered sequence for the common TSBPD delivery search and retains the full
search for out-of-order timestamp cases. It also advances `expected_seq`
directly for ordinary in-order packets; the contiguous buffered scan remains
for recovery of a sequence recorded in `loss_list`.

The focused Criterion benchmark measured the buffered 250ms TSBPD case at
`1.885ms` median after the combined change, versus `2.266ms` for the prior
candidate and `6.809ms` before the delivery hint. The empty-window control was
`1.485ms`, within the prior `1.461ms` control range. Protocol unit tests,
clippy with `-D warnings`, and the complete protocol property suite passed;
the out-of-order timestamp fallback test also remained green.

Manual whole-stack Rust/Rust MSR evidence with eight sink ports and eight Rust
sink workers is split by scale: 30/30 passed in one second with zero sender
drops, and 700/700 passed in 27 seconds with zero sender drops. The 1,200
attempt did not produce a checkpoint and remained in connection-admission
churn after more than four minutes, with repeated failure/disconnect activity;
that run was terminated and retained under
`.local/artifacts/msr-rust-sink-scale-ports-threads8-fastpath-20260819/`.
Therefore this optimization moves the proven Rust/Rust knee through 700 but
does not close the 1,200-output, 8Mbps, zero-drop gate.
