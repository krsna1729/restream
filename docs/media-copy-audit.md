# Media Copy Audit and Allocator Exploration

Working record for removing avoidable byte copies from the RTMP/RTMPS/SRT
media paths now that `rml_rtmp` is vendored (`vendor/rml_rtmp`), plus the
Rust Allocator API exploration. Written so another agent can take over:
each item states evidence, status, and the next action.

Host for all numbers: 6-CPU KVM VPS, `--no-netns`, one 8 Mbit/s ingest.

## Contents

- [Rules this audit follows](#rules-this-audit-follows)
- [Instruction-level decision: memcpy and byte search](#instruction-level-decision-memcpy-and-byte-search)
- [Copy inventory](#copy-inventory)
- [Work items and status](#work-items-and-status)
- [Rust Allocator API exploration](#rust-allocator-api-exploration)
- [How to measure](#how-to-measure)

## Rules this audit follows

- Remove a copy only when owned code can hand over the buffer instead; every
  copy that remains has a written justification below.
- Attribute before changing: `perf record --call-graph dwarf` on the 100-output
  fan-out (see [How to measure](#how-to-measure)). On x86-64 glibc, `memcpy`
  resolves to `__memmove_avx_unaligned_erms`, so a "memmove" hotspot is every
  copy, not only overlapping moves.
- Verify with an interleaved live A/B plus a profile; microbenchmarks alone do
  not reach `pub(crate)` egress code.

## Instruction-level decision: memcpy and byte search

`benches/simd_alternatives.rs` (decision bench, run on demand) on this host:

| copy size | `copy_from_slice` (glibc AVX/ERMS) | `pulp` | `wide` |
|---:|---:|---:|---:|
| 1 KiB | 68.6 GiB/s | 59.3 | 38.2 |
| 8 KiB | 87.0 GiB/s | 58.5 | 36.2 |
| 64 KiB | 45.1 GiB/s | 44.1 | 36.2 |
| 512 KiB | 37.3 GiB/s | 36.6 | 30.7 |

Byte search (`0x47`, full-scan miss): `memchr` 16.5 ns / 83.5 ns / 744 ns at
1 KiB / 8 KiB / 64 KiB, versus `wide` 36 ns / 323 ns / 2.5 µs and `pulp`
331 ns / 2.9 µs / 23.4 µs; scalar is ~30x slower than `memchr`.

Decision: keep `copy_from_slice`/`extend_from_slice` for copies and `memchr`
(runtime AVX2 dispatch) for byte search. Both are already what the code uses;
the only lever left is removing copies.

## Copy inventory

Per received or sent media byte. "Per output" copies scale with fan-out and
are what matter; ingest copies scale with ingest bitrate only.

### RTMP ingest (vendored `rml_rtmp` + `src/media/rtmp/ingest.rs`)

| # | Copy | Status / justification |
|---|---|---|
| I1 | kernel → user read buffer | Required (io_uring read). |
| I2 | read buffer → deserializer `BytesMut` (`get_next_message` `extend_from_slice`) | **Removed:** `ServerSession::take_input_buffer` / `handle_buffered_input` let the socket append straight into the deserializer buffer (Compio `AsyncReadExt::append`: plain `read` overwrites from offset 0 and corrupted leftover partial chunks, which `aggregate_parser_budget_rejects_the_connection_that_exceeds_it` caught). Leftover partial chunks are reclaimed in place by `BytesMut::reserve`. |
| I3 | chunk payload → `current_payload_data` reassembly | Kept. Multi-chunk messages need chunk headers stripped into one contiguous payload. For single-chunk messages a zero-copy `split_to().freeze()` was rejected: the frozen `Bytes` would pin the whole read allocation (≥4 KiB) behind every ~200-byte audio frame for as long as the ring retains it (~20x memory amplification, outside `ParserBudget` accounting). |
| — | message → `RtmpMessage::{Audio,Video}Data` → `MediaPacket` | Already zero-copy (`Bytes` moves / refcount). |

### Raw feed → RTMP egress (`src/media/rtmp/egress_engine.rs`)

A Raw feed (SRT/TS ingest, transcoder output) carries Annex B + ADTS; RTMP
needs AVCC/HVCC + raw AAC.

| # | Copy | Status / justification |
|---|---|---|
| E0 | Annex B → AVCC conversion into a scratch `Vec`, then `Bytes::copy_from_slice` | **Fixed:** `src/media/rtmp/egress_payload_cache.rs`, a per-shard FIFO of the 64 most recent conversions (64 packets, audio included, each holding source plus converted payload; retained while the feed is idle), searched newest-first and keyed by source payload identity plus timing flags. Converts once per shard and writes straight into the buffer that becomes the shared `Bytes`, so the second copy is gone. The parameter-set scan runs once per shard per packet instead of once per output; outputs only copy the result when a packet carries parameter sets. Profile at 100 outputs: `RtmpMediaEncoder::encode` 10.9% inclusive → conversion 0.9% inclusive; memcpy share 8.3% → 4.5%. |
| E0' | FLV feed (RTMP ingest) | Already zero-copy (`packet.payload.clone()`). |

### RTMP/RTMPS egress transport (`src/media/egress/backends/compio_tcp/stream.rs`)

| # | Copy | Status / justification |
|---|---|---|
| T1 | chunk headers + payload slices → 64 KiB `outgoing` `Vec<u8>` (`write_vectored`) | **Removed:** staging is `Vec<Bytes>`: shared payload slices (`RtmpWireMessage::fill_parts`, `Bytes::slice_ref` of the message payload) plus sealed `BytesMut` runs of copied bytes, sent with one io_uring `writev` via `CompioTcpStream::write_shared` / `RtmpConnection::write_shared`. The 64 KiB `pending_write_bytes` backpressure is unchanged. |
| T2 | partial write → `data.drain(..n)` memmove of the unsent tail | Replaced by the same change: finished segments leave the list; the first unfinished one `advance`s. |
| T3 | user → kernel send | Required. Candidate follow-up: `MSG_ZEROCOPY`/`IORING_OP_SEND_ZC`, but completion notifications cost more than a copy for sends under ~10 KiB. Measure before adopting. |
| T4 | pre-kTLS rustls records | Kept: rustls encrypts into its own buffers; kTLS takes over after the handshake. |
| T5 | chunk headers (1–12 bytes) | Kept as copies: an iovec per header costs more than the copy. Shared slices under 512 bytes are also copied (`SHARE_MIN_BYTES`). |

### SRT egress (`srt-rs`, pinned git rev in `Cargo.toml`)

Profiled at `40ebacde`: SRT H.264 ingest → 10 SRT outputs (mediamtx
receivers), `perf -F 499` with DWARF stacks and kernel symbols, 7.2K
samples. Copies are not the SRT cost:

| Cost | Share | Where | Lever (all in srt-rs) |
|---|---:|---|---|
| userspace memcpy | ~1% | `PendingDatagram::encode_into` 0.26% (payload into the TX datagram), compio completion plumbing | Low value; a header + payload iovec send would remove the 0.26%. |
| kernel spinlock: receiver wakeup on our send | 2.7% + 0.96% | `udp_sendmsg` → loopback → `__udp_enqueue_schedule_skb` → `sock_def_readable` → `__wake_up_sync_key` (+ mediamtx `ep_poll_callback`) | On loopback the sender pays the receiver's wakeup (partly a test-topology artifact). UDP GSO / `sendmmsg` batching cuts per-packet sends and wakeups; measure on loopback and a real NIC. |
| io_uring timed waits | ~2% | `io_cqring_wait` → `schedule_hrtimeout` → `hrtimer_try_to_cancel` (+ `task_work_run` lock 1.56%) | Fewer timed waits in the Owner loop (wait without a timeout when no SRT timer is due, or coarser deadlines). |
| SipHash in `CallerTable` maps | ~1% | `poll_outbound_bounded_to_with_visits`, `sync_deadline`, `feed`, `send_shared` | A non-cryptographic hasher for internal id-keyed maps (ids are not attacker-chosen), or dense `Vec` indexing by `LogicalCallerId`. Add a hasher bench before choosing. |

Next action: take these into the srt-rs repo as separate, benchmarked
changes (Stage-B bench surface: `wi3-owner-bench` feature), then bump the pin
here and rerun the SRT fan-out A/B. Measure SRT fan-out against the harness sink
(`MSR_PEER=sink`): mediamtx is the bottleneck for SRT (roadmap §36).

### srt-rs backlog (evidence-backed)

srt-rs is ours (`/home/dev/srt-rs`, pinned in `Cargo.toml`). Evidence comes
from the SRT fan-out profile (SRT H.264 → 10 SRT outputs, `perf -F 499`,
DWARF stacks, kernel symbols; 7.2K samples, `40ebacde`) and the capacity
ramp into harness sinks (Restream pinned to 3 CPUs, `6616fa85`).

Context (baseline ramp, `docs/capacity-ramp.md`, 3 repeats, 30 s windows,
Restream on 3 pinned CPUs): capacity RTMP 1000, RTMPS 1000, SRT 50 outputs;
SRT 100 passed 2/3 and SRT 150 collapsed to 0/3. SRT costs ~2.5% of a core
per 8 Mbit/s output versus RTMP ~0.14%, and the SRT sink stayed within its
budget (Restream-limited).

| # | Fix (srt-rs) | Evidence | Expected effect |
|---|---|---|---|
| S1 | **Batch sends: one io_uring `sendmsg` with UDP GSO (`UDP_SEGMENT`) per peer burst**, instead of one `send_to` per datagram. The Compio Owner's `TxEngine` runs up to 64 lane tasks (`runtimes/compio.rs` `tx_lane_worker`), each awaiting `job.sock.send_to(job.buf, job.peer)` for a single datagram. srt-rs already has `sendmmsg` batching (`socket_io::sendmsg_batch`), but only the non-Compio paths use it. | `io_uring_enter` 43.1% inclusive, `io_sendmsg` → `udp_sendmsg` 26.2% / 23.6% (one kernel send path per ~1.3 KB datagram), `io_submit_sqes` 28.1%. RTMP sends up to 64 KiB per operation and costs ~9x less per byte. | Biggest lever: amortizes the per-datagram syscall, IP output and SQE/CQE handling across a burst. Must respect pacing: batch only packets already due (for example within one pacing tick). Data packets carrying 7×188-byte TS are equal-sized, which GSO needs. Measure on loopback and a real NIC. |
| S2 | **Wait without arming a timer when a CQE is already imminent**: `Owner::wait` wraps the activity `poll_fn` in `compio::time::timeout(timeout, …)` on every iteration (`runtimes/compio.rs` ~4106), so each wake arms and cancels an hrtimer. | `io_cqring_wait` 13.9% inclusive; `schedule_hrtimeout_range_clock` 11.3% inclusive / 2.2% self; `hrtimer_try_to_cancel` chains ~1.9%; `task_work_run` lock 1.56%. | Poll first and wait untimed when TX completions are outstanding (they wake the Owner). Otherwise use the protocol deadline with coarse, reusable timers. Worth ~2–5% at 10 outputs, more at scale. |
| S3 | **Replace SipHash maps on the per-packet path**: `CallerTable` `sessions: HashMap<LogicalCallerId, CallerSession>`, `routes: HashMap<u32, CallerRoute>`, `sched: HashMap<LogicalCallerId, SchedEntry>` (`caller.rs` ~412–417) use std's SipHash, although the keys are internal ids and not attacker-chosen. | `hash_one`/SipHasher 1.47% of samples, from `poll_outbound_bounded_to_with_visits` 0.32%, `sync_deadline` 0.22%, `feed` 0.21%, `send_shared` 0.10%. | Dense `Vec` indexing by `LogicalCallerId` (slab), or a fast non-cryptographic hasher. `routes` is keyed by a peer-chosen socket id, so keep DoS resistance there (for example a keyed fast hasher). Add a micro-bench first. |
| S4 | **Avoid the payload copy into the datagram**: `PendingDatagram::encode_into` copies header and payload into one TX buffer per datagram, because `send_to` takes a contiguous buffer. | memcpy leaf 0.26% under `encode_into` (libc memcpy+malloc 5.4% total). | With S1's `sendmsg`, pass header and payload as iovecs (payload stays `Bytes`). Small on its own; lands with S1. |
| S5 | **Expose peer-acknowledged payload directly** (`SenderStats`): Restream derives "delivered" as `total_bytes_sent − total_bytes_dropped − payload_bytes_in_buffer`, which leans on internal accounting (first-send counting, tombstones), and a receiver's own TLPKTDROP after ACK is invisible to it. | Review of `702bc3df`: SRT Restream-vs-receiver disagreement (SRT×100 into mediamtx: receiver 0.007, Restream 0.83). | An `acked_payload_bytes` counter, plus a receiver-drop estimate if the peer reports one, so delivery is not an upper bound by construction. |
| S6 | **Public stable id for `LogicalPeerId`** (`as_u64` is `pub(crate)`), for telemetry keys. | The harness sink had to invent a per-pool sequence to key per-connection bytes (`e521492b`). | Minor API. |
| S7 | **Re-verify the frozen-destination RSS regression after the Owner cutover**. It was recorded at the Compio pivot: `fault.srt-output-stall` RSS growth 78–93 MB versus 46–54 MB before, suspected in `CallerLeg::send_shared` queuing into the protocol output queue (16 MiB / 8192 actions per connection). | Session memory note from `ec8832d9`; not re-measured since. | Rerun `fault.srt-output-stall`; fix only if it still reproduces. |

**Status (srt-rs branch `perf/owner-tx-efficiency` at `d81e958`, pinned by
Restream).** Fixed-load A/B: SRT H.264 → 50 SRT outputs into harness sinks,
Restream on 3 pinned CPUs, 5 interleaved reps, baseline `10ef9b65`.

| # | Outcome | Evidence |
|---|---|---|
| S1 | **Done** (`d81e958`): same-socket, same-peer, equal-length datagrams committed in one Owner visit join the staged job and leave as one `sendmsg` with `UDP_SEGMENT` (≤ 44 segments / 60 KB). Per-datagram slots, completion policy and in-flight counts are unchanged. A GSO rejection turns coalescing off and counts transient losses, never an Owner fault. | Restream CPU median 129.9% → **106.3%** (−18%), delivery 50/50 in all 10 runs. `io_sendmsg` 43.6% → 28.7% of samples (≈ 57 → 31 points of a core); what remains is mostly the loopback receive softirq charged to our send. New srt-rs tests include a real-socket GSO round trip. |
| S2 | **Tried and rejected.** Waking on a TX completion only under TX pressure left completions unreaped, so their pool slots stayed out and the next burst had a smaller window. | Restream `one_ready_batch_is_one_owner_service_pass`: ≤ 2 → 16 Owner service passes. Completion wakes are unchanged; S1 already cuts completions per datagram. |
| S3 | **Done** (`d4bd860`): `IdHashMap` (multiply-rotate) for `CallerTable` sessions/routes/sched, whose keys are ids srt-rs allocates, so hash flooding is impossible. | Criterion `caller_table_scheduling` vs main, median: reschedule −72..−76%, all_ready −37..−61%, sparse_ready −31..−40%, one_due −16..−41%, one_ready −9..−48%, churn −5..−20%, idle_poll noise. |
| S4 | **Declined on evidence.** | Remaining memcpy is ~1.2% of samples in the S1 build, so the payload copy is ≤ ~1%. Removing it changes srt-proto's slot contract (and with AES the copy is the ciphertext output). It cannot meet srt-rs's own ≥ 5% live-sentinel retention bar. |
| S5 | **Done** (`e0e7af8`): `SenderStats::total_bytes_acked`; Restream's SRT delivery uses it. | Unit test `acked_bytes_count_live_payload_retired_by_peer_acks`; inline footprint +8 bytes, recorded. |
| S6 | **Done** (`d4bd860`): `LogicalCallerId::as_u64` / `LogicalPeerId::as_u64` public. | — |
| S7 | **Reproduced, then fixed in Restream.** The frozen-destination surge was pre-existing (baseline growth 71.6 MB, S1 build 66–72 MB, gate limit 64 MB). RSS sampling shows a plateau, not a leak. About half was glibc per-thread arenas keeping freed memory resident. Restream now calls `mallopt(M_ARENA_MAX, 2)` at startup unless `MALLOC_ARENA_MAX` is set. | Gate: growth 46.6 / 30.5 MB, **PASS ×2**. Arena cap A/B: SRT×50 CPU 103.6% → 101.3% (3 reps), RTMP×500 100.0% → 95.2% median (7 reps, noise), RSS −34 / −47 MB. An idle orphaned Restream from a heaptrack attempt ran during that A/B (both arms interleaved). |
| — | Also fixed: `compio_production_fanout` lacked `harness = false`, so it never ran. **Open:** with it running, it submits 0 datagrams at fan-out ≥ 100. | srt-rs `d81e958` message. |

Restream overload collapse: **done in Restream**. The SRT stall sweep treats
a shard where at least four, and at least 25%, of the previous sweep's
outputs were backpressured or stalled as saturated (the minimum is on
pressured outputs, so 1 stuck of 4 is still recycled; corrected after review,
the first version required only 4 visited). Stalled outputs are then kept connected
(`backpressureReason: "shard_saturated"`) instead of being force-closed and
reconnected into the same Owner. A lone stuck destination is still recycled
(unit tests), and `fault.srt-output-stall` passes.

Not srt-rs, but found in the same runs:

- **Restream overload collapse (policy).** At 200–400 SRT outputs on 3 CPUs,
  Restream's stall sweep closed 401 leaves ("no progress (stalled)", lag
  ≈ 300 units, some `blocked`), producing 586 "output failed" retries and 168
  peer disconnects within ~50 s. Reconnects add load to a saturated Owner, so
  delivery collapses for nearly every destination instead of degrading for a
  few. Needs overload-aware stall handling (for example, do not reconnect
  into a saturated shard; shed the newest outputs) before or alongside S1.
- **Healthy SRT memory is fine.** About 0.35 MB per output at 100 outputs
  (RSS 163 → 194 MB from 10 to 100), against ~0.13–0.16 MB for RTMP. Growth to
  ~1.4 MB per output appears only under overload backlog.
- **Loopback receive cost.** ~11% of the profile is the loopback receive
  softirq (`net_rx_action` → `udp_rcv`) charged to our send; on a real NIC it
  lands elsewhere. Compare S1 on loopback and on a NIC or veth pair
  (`scripts/harness/veth-topology.sh`).

Order: S1 (with S4), then S2, then S3, re-running
`scripts/harness/capacity-ramp.sh` with `CAPACITY_PROTOCOLS=srt` after each.
The Restream collapse policy can proceed in parallel.

## Work items and status

State as of this writing, in order:

1. **Payload cache (E0): committed.** Unit tests in
   `egress_payload_cache::tests` cover exact equality with the production
   conversion, one conversion shared by three outputs, no false sharing, and
   eviction correctness. SRT H.264 → 100 RTMP outputs, 10 interleaved reps per
   variant against `702bc3df`: Restream CPU median 48.2% → 44.0% (mean 47.4% →
   44.1%), RSS median 209 → 189 MiB. Delivery: 9/10 runs had all 100
   destinations ≥ 0.95 versus 10/10 for baseline. The one dip (86/100) was
   uniform across destinations (Jain 0.99978), and Restream's own telemetry
   saw it too; it coincided with a CPU spike to 91% and 23 involuntary
   switches/s (host preemption burst). The worst-destination receiver ratio
   spread wider on the cache build (0.892–0.994 vs 0.970–0.980). **Watch
   item:** still open. `40ebacde`'s A/B cannot close it (both arms contain
   the cache); rerun cache vs `702bc3df` into the harness sinks if dips recur.
2. **Ingest direct read (I2):** see item 3.
3. **Zero-copy TX (T1/T2) and ingest direct read (I2): committed together.**
   Correctness: `mixed.live.rtmp.h264.a1.bf2` (18/18 outputs, sink probes
   pass) and `mixed.live.srt.h264.a2.bf0` (38/38) pass; full `cargo test`
   passes. 5 interleaved reps at 100 outputs against `95dd6355`:
   - RTMP (SRT H.264 ingest): CPU median 47.2% → 39.4% (mean 46.8 → 42.5),
     RSS 189 → 176 MiB, delivery 100/100 in every run.
   - RTMPS/kTLS: CPU median 54.6% → 57.2%, mean 56.7 → 56.3 (runs span
     48–70%: no measurable change), RSS 189 → 176 MiB, delivery 100/100.
     Profile (4K samples each): userspace memcpy 3.59% → 0.73%, libc
     9.9% → 8.2%, kernel share ~74% → ~76%. Kernel symbols were unresolvable
     (`kptr_restrict=1` at record time); to attribute kTLS cost, record with
     `kernel.kptr_restrict=0` (restore afterwards).
   The earlier ingest change (item 2) was folded into this commit.
4. **SRT egress audit: profiled** (see the SRT egress section). The work
   moves to srt-rs.

## Rust Allocator API exploration

What changed: the `Allocator` trait is stabilized for Rust 1.100
([rust-lang/rust#156882](https://github.com/rust-lang/rust/pull/156882)).
The MVP is deliberately small: a dyn-compatible two-method trait
(`allocate(Layout)`, `unsafe deallocate`), usable with `Vec` and `Box`
(`Vec::new_in`, `Box::new_in`); unwinding from allocators is prohibited;
`Box::pin_in` and other collections are not part of the first cut. The repo
pins 1.96.0 (`rust-toolchain.toml`), so none of this is usable until the pin
moves to ≥1.100 on stable. Do not adopt it through nightly.

Where Restream stands today:

- Production uses the system allocator (glibc `malloc`); the
  `#[global_allocator]` in `src/lib.rs` is a `#[cfg(test)]` counting allocator.
- In the 100-output fan-out profile the malloc family was ~8% of Restream CPU
  (`malloc`, `_int_malloc`, `malloc_consolidate`, `_int_free`,
  `unlink_chunk`, `cfree`). Part of that comes from the harness's API polling
  (serde_json/BTreeMap telemetry), not the media path; callers still need
  attributing.
- Media payloads are `bytes::Bytes` end to end (ring, feeds, rml_rtmp
  messages, srt-rs send buffers). `Bytes`/`BytesMut` do not take an allocator
  parameter, so the Allocator API does not reach them directly.
  `Bytes::from_owner(Vec<u8, A>)` (bytes ≥1.9; the tree has 1.12.1) can wrap
  an allocator-backed buffer.

Leverage points, in the order to try them:

1. **Attribute malloc now (no API needed).** `perf report` callers of
   `_int_malloc`/`_int_free` on the fan-out profile; separate the media path
   from API/telemetry.
2. **Global allocator A/B now (no API needed).** mimalloc or jemalloc as
   `#[global_allocator]`, interleaved fan-out A/B for CPU and RSS. This is the
   cheapest test of whether allocation cost matters at all.
3. **Per-shard allocators for thread-confined transient `Vec`s (≥1.100).**
   Candidates: `split_annexb_nalus`' `Vec`, rml_rtmp's
   `Vec<ServerSessionResult>` per `handle_input`, engine action vectors, the
   payload cache's conversion buffer. Only for memory that never leaves the
   shard thread.
4. **Shared payloads from a slab (≥1.100, careful).** A `Vec<u8, SlabAlloc>`
   wrapped with `Bytes::from_owner` can back converted payloads. `Bytes` may
   be dropped on any thread (ring readers, other shards), so the allocator's
   `deallocate` must be thread-safe. A thread-local bump arena is unsound here.
5. **io_uring fixed buffers (≥1.100).** `compio-buf` already has an
   `allocator_api` feature (`IoBuf`/`IoBufMut` for `Vec<T, A>`). An allocator
   carving from memory registered with `io_uring_register_buffers` would allow
   `READ_FIXED`/`WRITE_FIXED` for ingest reads and copied TX runs, avoiding
   per-op page pinning. This needs compio fixed-buffer support to be checked.
6. **Vendored `rml_rtmp`.** Its internal buffers are `BytesMut`, so the
   allocator only helps its `Vec` scratch (results, AMF encoding); low value
   until item 1 shows it matters.
7. **srt-rs.** Same `Bytes` constraint for payloads; loss lists and maps are
   `BTreeMap`, which the MVP does not cover. Revisit when collection support
   lands.

Do not start 3–7 before 1–2 show allocation is a real share of media-path CPU.

**Result of step 1 (malloc attribution, after `40ebacde`).** SRT H.264 →
100 RTMPS outputs, 4.3K samples (`perf script` stacks folded by leaf
symbol): the malloc/free family is 3.0% of Restream samples. Of that, 77%
runs on `restream-tokio` (API/telemetry JSON serving the harness's
once-per-second polling: `serde` serialization, health/telemetry/system
snapshots, hyper writes). Egress shard and SRT ingest threads together are
~0.4% of samples. The earlier ~8% (RTMP profile, before `95dd6355`) was never broken down by
caller; that it was mostly per-output conversion `Vec`s is an inference from
the payload cache removing them, not a measurement. Conclusion: allocation is not
a media-path cost worth an allocator change today. Steps 2–7 are parked
until a profile shows media-thread allocation above ~2%. If API polling cost
matters for operators, reduce allocation in the telemetry JSON builders
first; that is ordinary code, not an allocator question.

## How to measure

- Profile: there is no canonical script yet. The recipe runs
  `test_harness resource-sweep` with `RESOURCE_SWEEP_SCENARIOS=egress-growth-source-same`
  (SRT H.264 ingest → RTMP outputs), `RESOURCE_SWEEP_EGRESS_COUNTS=100`, waits
  for `outputs=100/100`, then
  `perf record -F 199 --call-graph dwarf,16384 -p <restream pid> -- sleep 12`.
  `perf` needs `kernel.perf_event_paranoid` lowered (restore it afterwards).
  Use a longer window or a higher frequency when differences are small: 12 s
  at 199 Hz is only ~1K samples.
- A/B: interleave baseline and candidate binaries per repetition (same
  harness binary; only `RESTREAM_BIN` differs), ≥5 reps at the output count of
  interest. Report the median and range; this host's CPU figure swings ±15%
  between runs.
- Always record delivery (receiver ratio, Jain) next to CPU: a CPU win that
  loses delivery is a regression.
