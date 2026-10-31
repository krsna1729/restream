# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-027 SRT×100 harness sink drops](#q-027-performance-opus-the-harness-srt-sink-drops-datagrams-at-srt100)
- [Q-034 overload policy](#q-034-resilience-opus-define-one-overload-policy-for-shared-service-centers)
- [Q-047 MoQ egress: IETF MOQT profiles](#q-047-protocol-opus-moq-egress-ietf-moqt-profiles)
- [Q-070 MoQ ingest: IETF MOQT profiles](#q-070-protocol-opus-moq-ingest-ietf-moqt-profiles)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)
- [Q-091 SRT Backup replays the lost prefix](#q-091-protocol-opus-srt-backup-failover-replays-the-lost-prefix)
- [Q-096 GRO and arrival timestamps](#q-096-performance-sonnet-gro-and-arrival-timestamps-in-the-reference-srt-path)
- [Q-100 agent protocol: approval mid-task and live panels](#q-100-agent-opus-agent-protocol-approval-mid-task-and-live-panels)
- [Q-101 incident investigation and approval UX](#q-101-agent-opus-explain-an-incident-and-approve-its-fix-in-one-place)
- [Q-102 fuzzy command palette](#q-102-frontend-sonnet-fuzzy-command-palette)
- [Q-106 separate-rendition HLS push order](#q-106-testing-opus-separate-rendition-hls-push-depends-on-server-arrival-order)
- [Q-108 ring bench shift after the engine move](#q-108-performance-opus-explain-the-ring-producer-bench-shift-after-the-engine-move)
- [Q-109 SRT group receive skips a lost sequence](#q-109-protocol-opus-an-srt-group-receiver-skips-a-sequence-every-member-lost)
- [Q-110 chrono to jiff](#q-110-platform-sonnet-move-from-chrono-to-jiff-after-jiff-10)
- [Not planned](#not-planned)
  - [Q-033 host-level shard pool](#q-033-host-level-shard-pool)
  - [Q-071 HLS-to-HLS passthrough](#q-071-hls-to-hls-passthrough)
  - [Q-097 MediaMTX MoQ dialect interoperability](#q-097-mediamtx-moq-dialect-interoperability)

### Q-025 [performance] [opus] Re-qualify the SRT shard law after transport convergence
- Goal: qualify the SRT cold prior (64 outputs per shard at 4.8 Mbit/s,
  `crates/restream-engine/src/egress/sizing.rs`, [SRT shard capacity](runtime-crossings.md#srt-shard-capacity))
  and `srt_egress_connect_concurrency` (`crates/restream-engine/src/config.rs`) on a second host.
- Gates: the single-shard SRT matrix on another host; no production policy
  change without measured evidence.

### Q-027 [performance] [opus] The harness SRT sink drops datagrams at SRT×100
- Goal: SRT capacity rungs measure Restream, not the receiver.
- Evidence: with one sink Owner per port and outputs spread by name (#459),
  outputs land evenly and no sink Owner faults, but an idle-host
  `CAPACITY_PROTOCOLS=srt CAPACITY_SRT_OUTPUTS=100 CAPACITY_REPEATS=7
  CAPACITY_PEER_COUNT=2` run (Restream on CPUs 0-3, harness on 4-5) passed
  6 of 7. In the failing repeat the sink's kernel sockets dropped 221,320
  datagrams over its 4 ports and one destination fell to 0.908; Restream
  delivered. Host jitter on Restream's CPUs was 6.24% (worst 172 ms) with
  nothing else scheduled.
- Next: raise the sink's receive capacity on 2 CPUs (receive buffer,
  batched receive, ports per peer) until it reports no kernel drops at
  SRT×100.
- Gates: the run above passes 7 of 7 with zero sink kernel drops.

### Q-034 [resilience] [opus] Define one overload policy for shared service centers
- Goal: when a shared Owner or shard saturates, viable destinations keep
  delivering and the excess is refused with an operator-visible reason.
- Context: RTMP, SRT and MoQ refuse an Add that would push its shard above
  60% busy (`ShardSizer::admit`, `capacityRefused`; live gate
  `srt.admission`). Still open: HLS PUT, sink and pipeline fabrics have no
  admission, and the cold prior overprices a host until a healthy window
  measures it (Q-025).
- Gates: the same admission contract and a live saturation case for each
  remaining fabric.

### Q-047 [protocol] [opus] MoQ egress: IETF MOQT profiles
- Goal: MoQ egress and origin also speak IETF MOQT (MSF catalog, LOC
  objects) beside moq-lite + hang: drafts 16, 18 and 19 against moq-relay
  and the `moq` CLI (`--connect-version moq-transport-16/18/19`), drafts 16
  and 18 against moxygen.
- Done so far: wire-codec fuzzing (`moq_wire`, `moq_ingest`) and moxygen
  interop (`moq-moxygen-interop`, draft-16 and draft-18).
- Gates: a harness interop mode per draft against moq-relay with the `moq`
  CLI as subscriber; moxygen stays the independent wire check.

### Q-070 [protocol] [opus] MoQ ingest: IETF MOQT profiles
- Goal: an IETF MOQT publisher (MSF catalog, LOC objects) feeds a pipeline
  input through the existing MoQ listener. The stream key comes from the
  connection path, else the track namespace; the input registers as
  protocol `"moq"`.
- Publishers: GStreamer `moqsink` with `container=loc`, and the `moq` CLI
  through moq-relay.
- Gates: one harness mode ingesting from each publisher, with frame counts
  and monotonic timestamps checked on read-back.

### Q-074 [performance] [opus] A rare multi-millisecond HLS ingest poll is bounded whatever its cause
- Goal: a `hlsIngest` poll on the MEDIA executor never spends multiple
  milliseconds of thread CPU.
- Remaining work: run the live gate on a host whose thread CPU clock is
  trustworthy. The application bounds are in place: a box walk counts
  nested boxes, a poll converts at most 256 fMP4 samples, and a whole
  `moof` decode and plan takes about 0.5 ms at the 16384-sample bound on a
  Zen 1 VM, so a full incremental parser is not needed. On that VM the gate
  cannot pass or fail the code: TS and fMP4 both showed 19–40 ms maxima,
  about half of it system time, while the `--host-probe` thread, which does
  no work, saw thread-CPU gaps up to 97 ms (hypervisor time charged to the
  thread). Allocator or kernel tuning showed no repeatable tail improvement.
- Gates: release `hls-push-duty -- --no-netns --hls-publishers 8
  --duration-secs 60 --poll-usage --host-probe`, with `--segment-type ts`
  and `--segment-type fmp4`, on bare metal over five runs each, with
  `hlsIngest` max CPU below 4 ms and fewer than one poll above 4 ms per
  minute per session, and a probe whose own thread-CPU gaps stay below
  1 ms. A mean or p99 passing does not waive the maximum.

### Q-091 [protocol] [opus] SRT Backup failover replays the lost prefix
- Goal: when the active sender in an SRT Backup group disappears, the promoted
  standby replays the group's recent send history before new payload, so the
  receiver does not lose the interval the active issued but never delivered.
- Change (srt-rs, then bump the pinned rev in `Cargo.toml`): a bounded
  recent-send window per group, standby stability qualified by
  `max(T_min, 2*SRTT + 4*RTTVar)` (as in srtgo), replay on promotion, and
  refused promotion when the window cannot cover the gap.
- Gates: differential cases against srtgo's state model; a harness mode that
  kills the active SRT output mid-stream and checks receiver continuity.

### Q-096 [performance] [sonnet] GRO and arrival timestamps in the reference SRT path
- Goal: the non-Compio reference SRT path (srt-rs) accepts GRO-coalesced
  datagrams and stamps socket arrival time before queueing, so scheduler
  backlog does not distort inter-arrival facts.
- Gates: a unit test that a coalesced receive splits into logical datagrams
  with identical arrival stamps; a bench of per-datagram cost with and without
  GRO.

### Q-100 [agent] [opus] Agent protocol: approval mid-task and live panels
- Goal: the MCP server (`crates/restream-mcp/`) asks for input or approval during a
  plan (elicitation), reports progress on long operations, and returns small
  live panels (pipeline health, a plan diff with an Approve button).
- Gates: agent-plane tests drive an elicited approval end to end; a refused
  approval leaves no change; panel payloads are contract-tested.

### Q-101 [agent] [opus] Explain an incident and approve its fix in one place
- Goal: "Explain this incident" reads the incident's flight-recorder window,
  diagnoses it deterministically and proposes a fix as an agent-plane plan;
  approval shows what the change touches and approves one action at a time
  by role, in the product only.
- Files: `web/ts/control-room/incidents*.tsx`, `crates/restream-agent/src/plane/`.
- Gates: a recorded incident yields a plan that names the right layer; an
  approval from a role below the action's minimum is refused and audited.

### Q-102 [frontend] [sonnet] Fuzzy command palette
- Goal: the ⌘K palette matches loosely worded requests to the existing
  commands (fuzzy matching, no language model) and shows the resulting plan
  before anything runs.
- Files: `web/ts/control-room/shell/command-palette.tsx`, `palette-model.ts`.
- Gates: misspelt and reordered requests reach the same command the buttons
  would; nothing applies without the approval step.

### Q-106 [testing] [opus] Separate-rendition HLS push depends on server arrival order
- Goal: `ffmpeg_hls_with_a_separate_audio_rendition_publishes_both`
  (`tests/hls_push_ingest.rs`) failed once in the CI coverage job. FFmpeg
  with `-http_persistent 0` reads no replies, and a playlist goes through
  the blocking-pool parse while a segment does not, so the session's arrival
  order is the server's, not FFmpeg's.
- Finding: a deterministic replay of FFmpeg's own write order (from its
  `Opening '…' for writing` log) over one persistent connection fails every
  run: audio up to 8 s reaches the ring before video at 2 s, which suggests
  the session follows the audio rendition until FFmpeg's final master write.
  The replay test is parked at `.local/parked/q106-hls_push_ingest.rs`
  (local only). Decide whether that order is a product defect (a live
  separate-rendition encoder would see it) or a replay artefact, then fix
  the product or the replay.
- Gates: the test is deterministic (no wall-clock budget) and passes 200
  consecutive runs under `cargo llvm-cov` on a loaded host.

### Q-108 [performance] [opus] Explain the ring producer bench shift after the engine move
- Goal: `ring_buffer/producer/push_one_at_a_time/4` measured +17.5% and
  +21.5% after moving the benches into `crates/restream-engine` (A/B against
  `2fb586c1`), while a same-code A/A rerun drifted +2.4%. The bench source,
  `restream-sync` and `restream-core` code and their resolved features are
  unchanged, so binary layout is the suspected cause, but that is unproven.
  Compare `perf stat` counters and the disassembly of the push loop in both
  bench binaries and either confirm layout or find the cause.
- Gates: a written cause backed by counter or disassembly evidence; if the
  product path is affected, a fix with a before/after Criterion A/B.

### Q-109 [protocol] [opus] An SRT group receiver skips a sequence every member lost
- Goal: when no group member delivers the next sequence before its TSBPD
  deadline, the group receiver releases the following packet instead of
  waiting forever. Today `srt_group/mod.rs:564` (srt-rs `2119264`) returns
  `None` while `next_receive_sequence` is missing from `pending`, so one
  sequence lost on every link stalls the group.
- Change (srt-rs, then bump the pinned rev in `Cargo.toml`): skip to the
  oldest pending sequence once the missing one's deadline passes, and count
  the skip as a group drop.
- Gates: a deterministic srt-proto test in which every member loses one
  sequence and later packets are delivered in order with one counted drop.

### Q-110 [platform] [sonnet] Move from chrono to jiff after jiff 1.0
- Goal: no crate depends on chrono. Its maintainer has soft-deprecated it
  ([chrono#1768](https://github.com/chronotope/chrono/issues/1768)), and a
  RustSec "unmaintained" advisory waits for jiff 1.0; with no ignore in
  `deny.toml`, that advisory would fail the Cargo deny job.
- Scope: 5 crates and about 40 files, mostly `Utc::now`, RFC 3339 output
  and `from_timestamp` conversions; `serde_with`'s chrono feature also
  pulls it in.
- Gates: contract tests pin today's serialized timestamps (API JSON and
  SQLite rows) before the switch and pass unchanged after it;
  `cargo tree -i chrono` is empty.

## Not planned

### Q-033 host-level shard pool
A pooled prototype cut shard CPU at 16 feeds × 2 outputs by more than half,
but an injected Owner fault failed both feeds in a shared pool. Production
keeps per-feed shard groups
([per-feed shard isolation](runtime-crossings.md#per-feed-shard-isolation));
reopen only if that failure-domain contract changes.

### Q-071 HLS-to-HLS passthrough
Relaying pushed HLS segments past the ring would need separate capacity
accounting, preview and health for ring-less outputs, and segment-level
failover. No telemetry shows HLS demux plus segmenting as a meaningful cost.

### Q-097 MediaMTX MoQ dialect interoperability
Blocked upstream: MediaMTX v1.21.2 has no PUBLISH_NAMESPACE dispatch, waits
for the server's SETUP, puts one frame per group with codec-tick timestamps
and no timescale, and emits a non-MSF catalog. Restream carries no MediaMTX
profile; `moq-relay-interop` stays diagnostic-only.
