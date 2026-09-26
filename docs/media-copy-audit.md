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
