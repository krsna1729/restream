# Isolation audit

The rule: **one publisher or one destination never affects another.** This
audit lists every resource that entities share, the bound that keeps one
entity from taking it all, and the test that proves the bound. A row
without a bound or a test is an open finding. Each finding is closed in
the change that fixes it.

Audited against `master` at `62ccdafe` plus #237 (fault domains), October
2026. Panic containment is described in
[architecture: fault domains](architecture.md#fault-domains).

## Contents

- [Threat model](#threat-model)
- [Shared resources](#shared-resources)
- [Findings](#findings)
- [Compile-time guards](#compile-time-guards)
- [Order of work](#order-of-work)

## Threat model

An entity is one RTMP or SRT ingest connection or peer, or one egress
output (its destination server). It may be malicious or broken. It may:

- send any bytes, in any split, at any rate, including none;
- open as many connections as the kernel allows, from one address;
- stop reading (a destination) or stop sending (a publisher);
- hold valid credentials (a publisher with a real stream key).

It must not be able to: end or stall a thread other entities use, consume
a shared budget so others are refused, or corrupt state others read.

## Shared resources

| Resource | Shared by | Bound per entity | Bound in total | Proof |
|---|---|---|---|---|
| RTMP owner thread | all RTMP connections | panic boundary per connection | — | `a_panicking_connection_does_not_stop_the_listener` |
| RTMP accept loop | all RTMP clients | one accept in flight, never dropped | — | `clients_connecting_while_connections_end_are_all_accepted` |
| RTMP connection slots | all RTMP clients | `RESTREAM_RTMP_MAX_CONNECTIONS_PER_IP` 64 per IPv4 address or IPv6 /64, across owners | `RESTREAM_RTMP_MAX_CONNECTIONS` 512 | `one_client_cannot_take_every_connection_slot`, `client_slots` tests |
| RTMP time before publish | all RTMP clients (holds a slot) | handshake 10 s, then `RESTREAM_RTMP_PREAUTH_TIMEOUT_MS` 10 s to publish | — | `a_client_that_never_publishes_is_closed_at_the_admission_deadline` |
| RTMP parser memory | all RTMP connections on an owner | `RESTREAM_RTMP_MAX_MESSAGE_BYTES` 8 MiB per message | aggregate parser budget | `oversized_declared_message_rejects_the_publisher`, `aggregate_parser_budget_rejects_the_connection_that_exceeds_it` |
| RTMP control channel | one connection | 16 commands | control sessions ≤ connection cap | — (per connection by construction) |
| SRT Owner thread | all SRT peers | panic boundary per peer (media work) | — | `a_panicking_publisher_is_disconnected_and_the_owner_keeps_serving` |
| SRT protocol state (srt-rs) | all SRT peers | srt-rs: fuzzed, Miri, per-peer state | — | srt-rs CI |
| SRT peer slots | all SRT peers | `RESTREAM_SRT_MAX_PEERS_PER_IP` 64 (srt-rs `max_peers_per_ip`) | `max_peers` 4096, half-open 1024 (10 s) | srt-rs admission tests |
| SRT pre-admission media | unattached peers | 2.5 MB per peer | 32 MiB per Owner | `a_new_peer_keeps_its_first_payloads_when_the_admission_cap_is_full`, `a_peer_over_its_own_quota_drops_its_own_oldest_payloads` |
| SRT probe hold | publishers awaiting their probe | `probe_hold_peer_bytes`, timeout | `probe_hold_total_bytes` | `concurrent_stalled_probes_stay_inside_their_byte_budgets`, `discarding_a_panicked_peer_returns_only_its_held_bytes`, `a_replay_that_unwinds_leaves_no_bytes_charged` |
| SRT Owner → Tokio events | all SRT peers | — | 256 bridge + 1024 pending; media never waits | M3 measurements (`runtime-crossings.md`) |
| Egress shard thread | outputs on the shard | panic boundary per visit; `WorkBudget` per visit | — | `panicking_leaf_fails_alone_and_its_shard_keeps_serving_the_others`, `blocked_leaf_does_not_starve_ready_leaf_on_same_shard_thread` |
| Egress send memory | outputs on the shard | `LeafLimits::max_pending_bytes` 256 KiB, lag, backpressure time | shard leaf capacity | leaf isolation suite (`egress/shard/tests/leaf_isolation.rs`) |
| Std locks shared by entities (shard snapshot, ring reader registry, ingest security, HLS stores, …) | entities and the status API | poison-tolerant `crate::sync::{lock, read, write}`; raw calls disallowed by `clippy.toml` | — | `a_poisoned_lock_still_serves_later_callers`; workspace clippy `-D warnings` |
| Feed rings | one pipeline's outputs | readers never block the producer; overrun resyncs the reader | ring capacity | `fault_injection_rapid_overflow_recovery`, `first_visit_primes_the_cursor_to_the_latest_sync_point` |
| Listener tasks | the whole protocol | — | restart with backoff | `listener_supervisor` tests |
| Process allocator, FDs, CPU | everything | bounded by the per-entity bounds above | `nofile`, cgroup | capacity ramps |

## Findings

Ordered by severity: unauthenticated first, then by what is lost.

Closed: F1 RTMP per-client connection cap, F2 RTMP admission deadline, F3
SRT per-client cap, F5 lock poisoning (191 call sites; the 3 that panicked
were the egress shard snapshot, about 21 silently skipped their work once
poisoned), F4 below; see the table for bounds and tests.

- **F4 (closed). The MPEG-TS demuxer had no fuzz target.** `ts_demux`
  (`fuzz/fuzz_targets/ts_demux.rs`) drives `TsDemuxer` and, through it, the
  stream probe in `mpegts_probe`, seeded from the TS fixtures by
  `fuzz/seed-corpus.sh`: 1.08M runs, 1,601 edges, no crash. The `Parser
  fuzz smoke` CI job runs every target.
- **F6. Parsers of untrusted bytes still index and do arithmetic
  unchecked:** 276 indexing, 220 arithmetic and 66 truncating-cast sites in
  `rtmp/flv.rs`, `codec/`, `mpegts/demux.rs`, `mpegts_probe.rs`,
  `hls/fmp4/codec.rs` and the RTMP ingest modules. Each is a panic (contained
  now, but the entity still fails) or a silent wrap in release. Fix: convert
  module by module to checked forms and deny the lints there.
  Converted, with the lints denied: `rtmp/flv.rs`, `mpegts/demux.rs`,
  `mpegts_probe` (with `h264`, `h265`), `codec/{bits, h264_sps, video, aac,
  enhanced_rtmp_hevc}`, `hls/fmp4/codec.rs`, `rtmp/ingest_media.rs`,
  `rtmp/timestamps.rs`, `rtmp/ingest/parser_budget.rs`. The conversion also
  removed duplicate parsers that had drifted: one H.264 SPS parser
  (`codec::parse_h264_sps`), one bit reader and RBSP (`codec::bits`), one
  AVC decoder configuration walker (`codec::video::avcc_record`). Defects it
  found: 1080i read as 1920x1084 on RTMP; high profiles 134-139 read as
  32x5120 on TS; `avcc_to_annexb` panicked on a length width of 0; an AAC
  frame above 8,184 bytes got an ADTS header with a truncated length (the
  frame is now dropped).
- **F7. Release builds wrap on integer overflow** (no `overflow-checks`).
  An overflow that is a contained panic in tests is a wrong value in
  production (the SPS scaling-list bug was this). Decision needs data:
  benchmark the media hot paths with `overflow-checks = true`.

## Compile-time guards

What the compiler and clippy can enforce:

| Guard | Mechanism | Status |
|---|---|---|
| Containment needs unwinding | `#[cfg(panic = "abort")] compile_error!` in `lib.rs` | done |
| No panicking or skipping lock access | `clippy.toml` `disallowed-methods` on `Mutex::lock`/`RwLock::{read,write}` outside `crate::sync` (tests, benches and the harness may use them) | done (F5) |
| Parsers cannot index, overflow or unwrap | `#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects, clippy::unwrap_used, clippy::expect_used, clippy::panic)]` per parser module | per module (F6; converted modules listed there) |
| Overflow is never silent | `overflow-checks = true` in release | after benchmark (F7) |
| Fault-injection points never ship | `#[cfg(test)]` only (`INJECTED_PANIC_PAYLOAD`, `injected_panics`, `EngineScript::Panic`) | done |

## Order of work

1. F4 fuzz targets; fix any crash with a regression test (done).
2. F6 module by module, each with a benchmark when it is on the hot path.
3. F7 benchmark and decision.
