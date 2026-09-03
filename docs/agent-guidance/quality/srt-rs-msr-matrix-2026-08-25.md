# SRT-RS MSR Protocol-Mix Matrix and Egress Bottleneck Attribution — 2026-08-25

## Contents

- [Exact-pin rebaseline — 2026-08-28](#exact-pin-rebaseline--2026-08-28)
- [Historical results matrix — old pin](#historical-results-matrix--old-pin)
- [Where the pure-SRT ceiling actually is: attribution](#where-the-pure-srt-ceiling-actually-is-attribution)
- [Knob matrix: what srt-rs offers vs what the engine uses](#knob-matrix-what-srt-rs-offers-vs-what-the-engine-uses)
- [State-stuffing assessment (socket_id / cookie)](#state-stuffing-assessment-socket_id--cookie)
- [Architecture verdict](#architecture-verdict)
- [Next actions (ordered, each independently verifiable)](#next-actions-ordered-each-independently-verifiable)

This document preserves the original 2026-08-25 MSR matrix and adds a separate
2026-08-28 rebaseline for the cutover. Restream consumes only
`https://github.com/krsna1729/srt-rs`, moving from exact revision
`0821257b402b08219aaaf38a62f5fa655a7e4947` to exact revision
`7663f1a11f905e4ae17e7188ae4f90240cc0ca0b`. Fixture:
`bbb-1080p60-30a.mp4` (BBB 1080p60, 1 video + 30 AAC tracks, **7,993,015
bps** aggregate — the real 8 Mb/s envelope). Ladder: 30/200/600/1200
outputs, one pipeline, continuous lifecycle. Host loopback, `--no-netns`
(netns unavailable in this sandbox). 6 cores.

The historical matrix below ran on the bench-profile harness at `e1f1e769` and
therefore describes the old pin. It is not evidence for the new revision.


## Exact-pin rebaseline — 2026-08-28

The new revision was rebuilt and exercised with the canonical 8 Mb/s fixture.
These results are deliberately separate from the historical matrix:

| peer | 30 | 200 | 600 | interpretation |
|---|---|---|---|---|
| MediaMTX×4, SRT-only | PASS | FAIL | not reached | 200 registered, then path stalls/reconnects; 252 Restream fabric-leaf terminations make this a mixed sender/receiver failure, not a receiver-only result |
| native sink×8/thread4, SRT-only | PASS | transient | FAIL | 200 reached the progress and five-second sink gates but active egresses decayed 195→172; 600 peaked at 540 and decayed to 139 before the deterministic timeout |

At the failed native-sink checkpoint Restream recorded 2,764
`CommandChannelFull`, 9,590 unexpected fabric-leaf terminations, and 12,354
`egress.failed` events. Raising the 1,024-entry command queue would only hide
the retry storm, so queue capacity is not a cutover fix. The 30-output run was
stable; 200 is not yet a sustained-capacity claim.

Artifacts:
`.local/artifacts/msr-krsna-pin-7663f1a-mediamtx-srt-only/` and
`.local/artifacts/msr-krsna-pin-7663f1a-sink8-thread4-srt-only/`.

## Historical results matrix — old pin

| mix | peer | 30 | 200 | 600 | 1200 | first failing gate |
|---|---|---|---|---|---|---|
| rtmp-only | mediamtx×4 | PASS | PASS | PASS | PASS | — |
| canonical 95/5 | mediamtx×4 | PASS | PASS | PASS | FAIL | ffprobe SRT read-back: zero video in 12 s window |
| srt-every:2 (50/50) | mediamtx×4 | PASS | PASS | FAIL | n/a | MediaMTX bytesReceived stalled on ~59/150 SRT paths of instance 0 |
| srt-only | mediamtx×4 | PASS | PASS | FAIL* | not reached | ffprobe read I/O error |
| rtmp-only | sink×8/thr4 | PASS | PASS | PASS | PASS | — |
| canonical 95/5 | sink×8/thr4 | PASS | PASS | PASS | PASS | — |
| srt-every:2 (50/50) | sink×8/thr4 | PASS | PASS | FAIL | n/a | engine egress leaf deaths + feed-overrun resyncs |
| srt-only | sink×8/thr4 | PASS | PASS | FAIL* | not reached | outputs-progress timeout: oscillating 252–340/600 registered |

\* the 600 checkpoint itself never stabilized; the run aborted there.

### Historical MediaMTX attribution (old pin)

With `PEER_COUNT=4` peers assigned by `ordinal % 4`, every even ordinal
under `srt-every:2` is SRT → all 600 SRT outputs land on **peer instance 0**
(one libsrt listener holding 150 live SRT conns while instances 1–3 hold
only RTMP). The stalled-path evidence matches exactly: every stalled path
mapped to instance 0. This is the same receiver-concentration knee the
[srt-scaling investigation](srt-scaling-investigation.md) measured (~700
connections best-case across a tuned pool, concentrated far worse). Not an
engine defect: rtmp-only passed clean through 1200 on the identical stack,
and the native-sink run of the same mix moved the failure from "receiver
stalls" to "engine egress" (below).

The canonical 95/5 mediamtx failure was a single sampled SRT read
(`msr-rank01-srt-0020`) delivering audio for its whole 12 s probe window
with zero video packets — healthy at 30/200/600 in the same run. Consistent
with the receiver's TLPKTDROP sacrificing large fragmented video PES under
transient pressure while small audio messages survive; the engine-side
progress gate had already passed for all 1200 outputs before the probe ran.

## Where the pure-SRT ceiling actually is: attribution

Instrumented live run at the 600-output srt-only checkpoint
(`MSR_NO_CLEANUP=1`, detached so it survived profiling):

| Measurement | Value | Reading |
|---|---|---|
| restream process total CPU | < 1.5 cores of 6 | **not CPU-bound** |
| mpstat host | 45 % idle, %sys 23 %, %soft 7 % | syscall/softirq heavy |
| perf top (restream, 999 Hz × 15 s) | 72 % kernel+libc; top app symbols BTreeMap insert/malloc family | allocation + syscall dominated |
| strace -c (8 s, all threads) | recvfrom 103,642; epoll_wait 17,948; sendto 5,234 | per-datagram syscalls |
| UDP /proc/net/snmp delta | InDatagrams 100–120k/s total | ≈ 26 % of the 456 k pps needed for 600 × 8 Mb/s |
| RcvbufErrors/s | 12–25 | not kernel socket-buffer loss |
| **egress shared-socket Recv-Q** | oscillating 0 → 168 KB (≈127 queued ACK datagrams) on restream's own sockets | **ACK servicing lag** |
| restream log | 4,613 `egress.failed` (fabric leaf terminated), 5,438 feed-overrun resyncs | consequence, not cause |

Causal chain (each step observed):

1. Engine egress drives leaves round-robin via `poll_leaves(0)` once per
   shard tick; each visit does bounded work (`OutputDrainBudget::default()`
   = 64 actions / 32 packets / 256 KiB).
2. With ~100+ leaves per shard, inter-visit latency per socket grows; the
   socket's inbound ACK queue builds between visits (`ss` Recv-Q to 168 KB).
3. Unread ACKs stall the ACK clock → sender flow window fills →
   `can_send()` false → TS ring cursor advances past the leaf →
   `FeedOverrun` resync (drops data, 5,438 logged).
4. Leaves that stay past the stall ceiling get torn down
   (`SRT fabric leaf terminated unexpectedly`, 4,613 logged) and retry with
   backoff — visible as the 252→340→252 progress oscillation.

The tokio adapter's TX path pays `sock.send(&bytes).await` **per datagram**
(the mio runtime path batches via `sendmmsg`; the tokio path does not), and
every protocol send does `payload.to_vec()` — one heap alloc + copy per
1316-byte packet per leaf, plus a clone per retransmission. At the target
456 k pps this is ~600 MB/s of memcpy and ~456 k allocs/s before any
protocol work; the profile's malloc/BTreeMap dominance is consistent.

For the cutover write-up: TSBPD, pacing, and ACK behavior improve in the new
pin; bonded shared-sequence handling also comes across in-scope. Preserve
CTR/default behavior, keep KM rejection and crypto hardening mandatory, and
treat optional AES-GCM plus input-bandwidth pacing as separate opt-in
follow-ups that still need their own measurement work. The remaining known
engine-side bottlenecks are unchanged here: tokio per-datagram sends and
protocol-side `Vec`/copy churn.

Why the harness sink passes where the engine egress doesn't: the sink only
receives and discards (no per-leaf ACK-clocked application send loop, no
TS-ring feed coupling), and its pool spreads load over 8 ports × 4 threads.
The engine must *send* 456 k paced datagrams/s through per-shard shared
sockets driven by a round-robin poller — a different, harder problem.

## Knob matrix: what srt-rs offers vs what the engine uses

| Knob (crate/API) | Default today | Available | Worth trying |
|---|---|---|---|
| `OutputDrainBudget` (transport) | 64 actions/32 pkts/256 KiB, hardcoded `::default()` at both call sites in `tokio_egress.rs` | constructor takes any budget | Tune up for TX-heavy leaves; budget-exhaustion then reschedules instead of starving |
| Tokio TX batching | per-datagram `.send().await` | mio path already has `sendmmsg` batching (`drain_outputs_with` + scratch); tokio `Conn::drain_outputs_bounded` does not | Port `sendmmsg` batching into the tokio drain (or use `try_send` loops over `Vec<SocketAddr>` batches). Highest-leverage single change |
| `payload.to_vec()` on push (protocol) | alloc+copy per packet, clone per retransmit | sans-I/O API owns bytes | Upstream: `Bytes`/`BytesMut` or caller-owned buffer pool through `SenderBuffer`. Removes most of the malloc profile |
| `SessionConfig` bandwidth/pacing | SRT pacing is always active; unspecified max bandwidth retains the protocol default ceiling, while input-bandwidth mode adds configurable overhead | bytes/s, bits/s, or measured input bandwidth plus 5–100% overhead | Defer configuration until Restream exposes a trustworthy per-output mux bitrate; guessing from fixture or declared profile risks under-pacing variable-rate output |
| `PeerTableConfig` (ingress/sink) | max_peers 4096, half-open 1024, per-IP 4096 | `PeerTable::with_config` | For 1200-connection runs raise table bounds above the connection count; per-IP bound matters on loopback where all peers share one IP |
| Cookie routing (`AdmissionOptions.cookie_routing`) | on | off switch exists (measurement only) | Keep on — `IngressTelemetry.cookie_routed` vs `stranded_conclusions` quantifies it; never disable in production |
| `recvmsg_batch` (transport free fn) | used by mio paths | available to any fd owner | Use in harness sink `drive()` receive burst (currently `try_recv_from` loop) — one syscall per 64 packets |
| CPU pinning (`restrict_to_cpu_list`) | unused by engine/harness | disjoint-set helper provided | Pin egress shards vs tokio workers vs sink pool on multi-core hosts; the scaling doc showed sender/receiver contention effects at 6 cores |
| `ListenerTopology::ReusePortMulti` | sink uses it | engine ingress too | Already proven; keep |

Non-goals kept as non-goals: changing wire semantics, weakening admission
(StreamID/KM checks stay mandatory), bypassing `catch_unwind` isolation, or
presenting AES-GCM / input-bandwidth pacing as default-on cutover behavior.

## State-stuffing assessment (socket_id / cookie)

Evaluated per the request: stuffing routing/state into `socket_id` (32-bit)
or SYN cookie (32-bit).

- **SYN cookie**: already carries worker index (low byte) + peer hash (upper
  24 bits) — `srt_lifecycle::cookie_for_worker`. It is explicitly documented
  as routing metadata, *not* a security boundary; adding more state would
  widen spoof/guess surface for zero measured gain (the CONCLUSION-routing
  cost is already paid once per handshake, not per datagram).
- **socket_id**: stable per-connection identifier echoed in every DATA
  header. Stuffing shard indices into it would couple wire identity to
  internal topology, break reconnect/rebind invariants (id must survive
  shard moves), and hand attackers a predictable handle. The per-datagram
  lookup it would "optimize" is a hash lookup in tables that are already
  FxHash-based and cache-resident; the profile shows the cost lives in
  syscalls and copies, not lookups.

Verdict: **do not stuff state into either field.** The measurable costs are
per-datagram syscalls and payload copies; both have direct fixes listed in
the knob matrix without touching wire-visible identity.

## Architecture verdict

Restream is not broadly over-engineered around the previous native SRT stack.
Keep the shared TS muxer, `TsChunkRing` fanout, lifecycle/retry policy, shard
ownership, desired-state manager, stream-ID admission, crypto policy, and feed
overrun/stall handling: those are product responsibilities, not transport
reimplementations.

The excess compatibility surface is concentrated at the adapter seam:
`SRTSOCKET = i32`, the global socket registry, an always-writable compatibility
poller, synthetic trace stats, and a private Tokio runtime hosted on SRT shard
threads. Contain that seam now; do not rewrite product lifecycle around it. The
target is a shard-owned transport using upstream connection/group types
directly. For high-density egress, use upstream Mio shared-socket batching as
implementation and measurement evidence; the product runtime is Tokio. For ingress, retain the current
listener until a scale experiment justifies `ReusePortMulti` or shared-pool
cookie routing.

The exact-pin `srt-bench` run is diagnostic, not acceptance evidence. Its custom
Mio/shared-socket profile established only one connection at 30 and 200, while
Tokio established 30/30 but only 88/200 callers at the larger point and recorded
UDP receive-buffer errors. Use the checked-in upstream sentinel plans and their
five-repetition/full-delivery method before making a runtime claim. Restream MSR
remains the product gate.

## Next actions (ordered, each independently verifiable)

1. **Land the exact pin and bonded correctness fix.** Shared `CallerTable` state
   is single-peer only; broadcast/backup outputs use upstream `TokioGroupConn`
   so group sequence state is not split across direct callers. Run focused bond,
   policy, concurrency, and full Rust gates.
2. **Add observability before tuning.** Record caller-table budget exhaustion,
   backpressure, retry reason/rate, active-versus-desired leaves, and per-shard
   visit latency. Do not enlarge the command channel or change the connect
   admission semaphore as a blind mitigation.
3. **Prototype a shard-owned high-density egress adapter.** First compare the
   exact-pin Mio per-connection/shared-socket sentinel with the current Tokio
   adapter at 30/200/600; then implement the winning upstream topology behind
   the existing transport boundary. Acceptance requires full delivery, clean
   teardown, and a Restream MSR improvement, not connection count alone.
4. **Tune `OutputDrainBudget` only with telemetry.** A larger budget can improve
   one leaf while starving neighbors; test a bounded matrix and retain the
   smallest value that lowers visit latency and leaf churn.
5. **Move payload ownership upstream.** Eliminate protocol-side per-packet
   `Vec` copies/clones through `Bytes` or equivalent ownership, then re-profile.
6. **Choose listener topology per workload.** Keep the proven ingress default;
   evaluate shared-pool/cookie routing and `ReusePortMulti` for dense listeners,
   including same-IP admission limits and teardown behavior.
7. **Add bandwidth hints only after measurement exists.** Feed a measured mux
   bitrate into upstream input-bandwidth mode with measured overhead. GCM and
   pacing configuration remain opt-in follow-ups, not cutover blockers.
8. Re-run the canonical 30/200/600/1200 ladder after steps 2–4; target sustained
   active egresses, near-zero send drops, no command retry storm, and no
   unexpected leaf terminations.

Artifacts: `.local/artifacts/msr-{rtmp-only,canonical,srt-every-2,srt-only}/`,
`.local/artifacts/msr-sink-{rtmp-only,canonical,srt-every-2,srt-only}/`,
`.local/artifacts/msr-perf*/`.
