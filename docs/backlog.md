# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-027 residual SRT×100 repeat failure](#q-027-performance-opus-find-why-srt100-still-fails-a-repeat-with-the-multi-owner-sink)
- [Q-033 host-level shard pool](#q-033-architecture-opus-run-egress-shards-from-one-host-level-pool-instead-of-per-feed-groups)
- [Q-034 overload policy](#q-034-resilience-opus-define-one-overload-policy-for-shared-service-centers)
- [Q-047 MoQ egress: IETF MOQT profiles](#q-047-protocol-opus-moq-egress-ietf-moqt-profiles)
- [Q-049 silent SRT leaves](#q-049-resilience-opus-find-why-connected-srt-leaves-sent-nothing-under-host-overload)
- [Q-066 SRT ingress test port race](#q-066-testing-sonnet-srt-multi-owner-test-port-can-join-a-foreign-reuseport-group)
- [Q-070 MoQ ingest: IETF MOQT profiles](#q-070-protocol-opus-moq-ingest-ietf-moqt-profiles)
- [Q-071 HLS-to-HLS passthrough](#q-071-architecture-sonnet-hls-to-hls-passthrough)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)
- [Q-091 SRT Backup replays the lost prefix](#q-091-protocol-opus-srt-backup-failover-replays-the-lost-prefix)
- [Q-092 no descendants of a dropped dependency](#q-092-resilience-opus-never-emit-descendants-of-a-dropped-dependency-bearing-unit)
- [Q-096 GRO and arrival timestamps](#q-096-performance-sonnet-gro-and-arrival-timestamps-in-the-reference-srt-path)
- [Q-097 MediaMTX MoQ dialect interoperability](#q-097-protocol-opus-align-mediamtx-moq-dialect-with-ietf-interop)
- [Q-100 agent protocol: approval mid-task and live panels](#q-100-agent-opus-agent-protocol-approval-mid-task-and-live-panels)
- [Q-101 incident investigation and approval UX](#q-101-agent-opus-explain-an-incident-and-approve-its-fix-in-one-place)
- [Q-102 plain-language command palette](#q-102-frontend-sonnet-plain-language-command-palette)
- [Q-104 FFmpeg 9 pin](#q-104-platform-sonnet-move-the-ffmpeg-pin-to-a-90x-point-release)
- [Q-106 intermittent timing tests](#q-106-testing-sonnet-timing-tests-fail-intermittently-on-a-loaded-host)
- [Q-107 matrix_throughput transcode benches](#q-107-testing-sonnet-matrix_throughput-transcode-benches-deliver-nothing)
- [Q-108 ring bench shift after the engine move](#q-108-performance-opus-explain-the-ring-producer-bench-shift-after-the-engine-move)

### Q-025 [performance] [opus] Re-qualify the SRT shard law after transport convergence
- Goal: qualify the SRT cold prior (64 outputs per shard at 4.8 Mbit/s,
  `crates/restream-engine/src/egress/sizing.rs`, [SRT shard capacity](runtime-crossings.md#srt-shard-capacity))
  and `srt_egress_connect_concurrency` (`crates/restream-engine/src/config.rs`) on a second host.
- Gates: the single-shard SRT matrix on another host; no production policy
  change without measured evidence.

### Q-027 [performance] [opus] Find why SRT×100 still fails a repeat with the multi-Owner sink
- Goal: SRT capacity rungs measure Restream, not the receiver.
- Context: with the two-Owner `SO_REUSEPORT` harness sink
  (`crates/test_harness/src/test_harness/harness_srt_sink.rs`), SRT×100 passed 6 of 7
  repeats. The failing repeat had 14 destinations at zero delivery and 5 leaves
  closed by the plain stall sweep (not shard saturation); kernel drops
  concentrated on one reuseport member per port. Hypothesis, unproven: one
  sink Owner stopped servicing its sessions. Probe with per-Owner session and
  ACK counters in the sink.
- Gates: `CAPACITY_PROTOCOLS=srt CAPACITY_SRT_OUTPUTS=100 CAPACITY_REPEATS=7
  CAPACITY_PEER_COUNT=2` (Restream on 4 CPUs, harness on 2) passes 7 of 7.

### Q-033 [architecture] [opus] Run egress shards from one host-level pool instead of per-feed groups
- Goal: shards are a host pool that serves every feed, not one group per
  (protocol, feed), without widening failure domains.
- Files: `crates/restream-engine/src/egress/runtime.rs`, `crates/restream-engine/src/engine_*_egress_fabric.rs`,
  `crates/restream-engine/src/egress/sizing.rs`, `crates/restream-engine/src/egress/backends/sink_shard.rs`.
- Context: a pooled prototype cut shard CPU at 16 feeds × 2 outputs by more
  than half, but an injected Owner fault failed both feeds in a shared pool
  ([per-feed shard isolation](runtime-crossings.md#per-feed-shard-isolation)).
  Production keeps per-feed groups until a design preserves the failure
  domains or that contract is explicitly changed.
- Gates: in the many-feed matrix, shard threads stop growing with feeds and
  per-feed shard CPU falls toward the per-output cost, every output
  delivered; frozen-destination, feed-teardown and structural Owner/shard
  faults keep their failure domains; loom for the shared publish wake
  (`WakeGate`).

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
- Goal: MoQ egress and origin also speak IETF MOQT (MSF catalog, LOC objects;
  `moqt-19` against moq-relay, drafts 16 and 18 against moxygen), Cloudflare's draft-16 (token in the URL path) and the MediaMTX
  v1.21.2 subset, beside moq-lite + hang.
- Done so far: wire-codec fuzzing (`moq_wire`, `moq_ingest`) and moxygen
  interop (`moq-moxygen-interop`, draft-16 and draft-18).
- Open: the MediaMTX subset (blocked on Q-097) and Cloudflare's draft-16 relay,
  which needs an account relay and a publish-capable token.
- Gates: a harness interop mode per peer.

### Q-049 [resilience] [opus] Find why connected SRT leaves sent nothing under host overload
- Evidence: under host CPU overload in `mixed.signal`, every SRT 1080p leaf of
  one pipeline completed the handshake and then sent no payload for 30 s,
  reporting `backpressured`, growing feed lag and no caller quality stats,
  while sibling SRT 720p and RTMP leaves delivered. It did not reproduce with
  CPU spinners.
- Question: can a connected leaf be starved by its shard or Owner while
  siblings progress? That would break output isolation.
- Leads: such a leaf should have been closed as `stalled` after 15 s
  (`classify_stall`, `egress/policy.rs`). It escapes that bound if it records
  progress without payload (`record_send` with units and no bytes, the
  backlog-shrink rule in `SrtLeaf::observe_stall`), has zero pending bytes or
  no caller stats (counts as idle), or sits on a `shard_saturated` shard.
  Next probes: the Owner's view of that caller, and which path fired.
- Gates: a reproduction or a proof that a connected leaf cannot stall
  silently; the fix keeps sibling leaves unaffected.

### Q-066 [testing] [sonnet] SRT multi-Owner test port can join a foreign reuseport group
- Goal: `two_owners_share_the_port_and_each_session_command_reaches_its_owner`
  cannot share its port with another process.
- Why: the test probes a free port and releases it before binding; a foreign
  same-uid `SO_REUSEPORT` socket (another `cargo test`) can take it in the gap
  and join the group. The byte counts then fail loudly. Fix with a per-test
  network namespace, or srt-rs binding Owner 0 first and the rest to its port.
- Gates: none until it flakes.

### Q-070 [protocol] [opus] MoQ ingest: IETF MOQT profiles
- Goal: an IETF MOQT `moqt-19` publisher (MSF catalog, LOC objects) feeds a
  pipeline input through the existing MoQ listener. The stream key comes from
  the connection path, else the track namespace; the input registers as
  protocol `"moq"`.
- Blocked: pinned moxygen has no MSF/LOC publisher and MediaMTX waits on
  Q-097. Closing waits for an upstream MSF/LOC publisher (user decision).
- Gates: interop with moxygen and MediaMTX as publishers (one harness mode).

### Q-071 [architecture] [sonnet] HLS-to-HLS passthrough
- Goal: an HLS PUT output relays the selected HLS input's segments into the
  `HlsStore` the shard already reads, skipping the ring, demuxer and segmenter.
- Conditions: no stage between input and output; matching container, codecs
  and segment boundaries; self-initializing input; validated at apply time
  with a fallback to the normal path and a stated reason.
- Costs: capacity accounting, preview and pipeline health need an answer for
  outputs that skip the ring; failover (`crates/restream-engine/src/input_gate.rs`) must be
  rebuilt at segment level (switch on a boundary, `EXT-X-DISCONTINUITY`,
  monotonic media sequence) or the relay must not ship.
- Trigger: telemetry showing meaningful CPU in HLS demux plus segmenting for
  HLS-to-HLS pipelines.
- Gates: a harness mode relaying pushed HLS, checked against a normal-path run;
  a test that an ineligible pipeline falls back with the reason recorded.

### Q-074 [performance] [opus] A rare multi-millisecond HLS ingest poll is bounded whatever its cause
- Goal: a `hlsIngest` poll on the MEDIA executor never spends multiple
  milliseconds of thread CPU.
- Remaining work: `shiguredo_mp4::handle_media_segment` decodes the whole
  `moof`, and sample planning runs in the poll that first reaches it. Make
  fMP4 parsing/planning incremental (`crates/restream-engine/src/hls_ingest/`,
  `crates/restream-engine/src/ingest_publisher/`), then verify TS and fMP4 poll budgets.
  Allocator or kernel tuning showed no repeatable tail improvement.
- Gates: an incremental fMP4 parsing/planning regression test; release
  `hls-push-duty -- --no-netns --hls-publishers 8 --duration-secs 60
  --poll-usage --host-probe` on a qualified host over five runs, with
  `hlsIngest` max CPU below 4 ms and fewer than one poll above 4 ms per
  minute per session. A mean or p99 passing does not waive the maximum.

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

### Q-092 [resilience] [opus] Never emit descendants of a dropped dependency-bearing unit
- Goal: every fanout boundary that drops a video unit under pressure (ring
  overrun, MoQ group eviction, WebTransport pressure, HLS discontinuity,
  recirculation, RTMP congestion) also suppresses dependent units until the
  next keyframe; where a boundary cannot see keyframes, document and test the
  fallback.
- Gates: a test per boundary that drops one unit and asserts no dependent unit
  is emitted before the keyframe; removing the suppression fails it.

### Q-096 [performance] [sonnet] GRO and arrival timestamps in the reference SRT path
- Goal: the non-Compio reference SRT path (srt-rs) accepts GRO-coalesced
  datagrams and stamps socket arrival time before queueing, so scheduler
  backlog does not distort inter-arrival facts.
- Gates: a unit test that a coalesced receive splits into logical datagrams
  with identical arrival stamps; a bench of per-datagram cost with and without
  GRO.

### Q-097 [protocol] [opus] Align MediaMTX MoQ dialect with IETF interop
- Goal: unmodified upstream MediaMTX interoperates with Restream's IETF
  MSF/LOC profile in both directions. No proprietary MediaMTX fork or profile.
- Blockers in MediaMTX v1.21.2 (pinned source, against MOQT-19, LOC-04 and
  MSF-00):
  - no PUBLISH_NAMESPACE (`0x6`) dispatch in `controlmessage.Read`;
  - `performSetup` waits for the server's SETUP, as moq-net's server does for
    the client's, so MediaMTX→Restream stalls;
  - one codec frame per group with a `0x06` timestamp in codec ticks and no
    timescale, where LOC uses `0x10`/`0x08` and MSF wants a GOP per group;
  - the catalog emits `channels`/`clockrate` (MSF: `timescale`,
    `channelConfig`) and `ToStream` accepts only `avc3`/`hev1`, not
    Restream's `avc1`/`hvc1`.
- Files: `crates/test_harness/src/test_harness/moq_relay_interop/`. Keep `moq-relay-interop`
  diagnostic-only; `moq-dev-interop --ietf` is the IETF gate.
- Gates: real egress and forwarded-ingest read-back with H.264/AAC, monotonic
  timestamps and frame counts against an unmodified upstream release; then
  enable the MediaMTX CI gate.

### Q-100 [agent] [opus] Agent protocol: approval mid-task and live panels
- Goal: the MCP server (`crates/restream-mcp/`) asks for input or approval during a
  plan (elicitation), reports progress on long operations, and returns small
  live panels (pipeline health, a plan diff with an Approve button).
- Gates: agent-plane tests drive an elicited approval end to end; a refused
  approval leaves no change; panel payloads are contract-tested.

### Q-101 [agent] [opus] Explain an incident and approve its fix in one place
- Goal: "Explain this incident" reads the incident's flight-recorder window
  and proposes a fix as an agent-plane plan; approval shows what the change
  touches, approves one action at a time by role, and can come from Slack or
  a push notification.
- Files: `web/ts/control-room/incidents*.tsx`, `crates/restream-agent/src/plane/`.
- Gates: a recorded incident yields a plan that names the right layer; an
  approval from a role below the action's minimum is refused and audited.

### Q-102 [frontend] [sonnet] Plain-language command palette
- Goal: the ⌘K palette also takes a request in words and acts through the
  agent plane, showing the plan before anything runs.
- Files: `web/ts/control-room/shell/command-palette.tsx`, `palette-model.ts`.
- Gates: a request maps to the same plan the buttons would make; nothing
  applies without the approval step.

### Q-104 [platform] [sonnet] Move the FFmpeg pin to a 9.0.x point release
- Goal: `restream ffmpeg-fetch` and the container images pin a BtbN 9.0.x
  build (a month's last, per [FFmpeg versions](ffmpeg-versions.md)) once a
  point release follows 9.0 "Lei". Nothing restream uses needs
  9.0; the move keeps the pin on a maintained branch. 9.0 turns `tls_verify`
  on and drops old NVENC options, neither of which restream uses.
- Gates: the startup capability check passes on the new build; the caption
  test (608 through a libx264 and a libx265 transcode, as in the
  `-a53cc` fix) still keeps every cue; the transport harness and the HEVC
  fixtures pass; an encode-speed A/B (libx264 and libx265, same host and
  profile) shows no regression.

### Q-106 [testing] [sonnet] Timing tests fail intermittently on a loaded host
- Goal: find why `the_projector_rebuilds_only_while_a_client_listens`
  (`src/application/runtime_state/tests.rs`) and
  `ffmpeg_hls_with_a_separate_audio_rendition_publishes_both`
  (`tests/hls_push_ingest.rs`) each failed once in the CI coverage job, and
  `a_playlist_parse_does_not_block_the_pull_task_worker`
  (`crates/restream-engine/src/hls_pull/tests/guards.rs`, 200 ms worker-delay bound) and
  `hevc_hls_preview_stage_uses_hevc_input_and_emits_h264`
  (`crates/restream-engine/src/external_transcoder/tests/hevc_pipeline.rs`, output deadline)
  each failed once in a full local `--lib` run; each passes when rerun.
  The xtask container tests (`crates/xtask/src/container/tests.rs`)
  `container_timeout_reaps_engine_client`,
  `container_shim_term_and_int_stop_container_then_reap_client` and
  `container_shim_cleans_named_container_after_engine_error` fail about 3%
  of full `xtask` test-binary runs (4 of 120 locally; twice in CI). A
  traced failure shows the fake engine shell not reaching its first
  statement before the 1 s deadline while the binary's other tests run in
  parallel; it is not ETXTBSY, and a fork-free fake engine still failed 5 of
  300. Remove the timing dependence.
- Gates: each test passes 200 consecutive runs under `cargo llvm-cov` on a
  loaded host, with no added sleeps or widened timeouts.

### Q-107 [testing] [sonnet] matrix_throughput transcode benches deliver nothing
- Goal: the `*_trans` cases of `crates/restream-engine/benches/matrix_throughput.rs` fail with
  `pulled=0` on master. The pinned FFmpeg child
  (`.restream/runtime/ffmpeg/ffmpeg`, libx265 720p) exits with `AVERROR_EOF`
  (-541478725) because its stdin closes before it opens the MPEG-TS input;
  the bench waits only 50 ms after starting the transcoder (around lines
  202-215). Make every transcode case deliver.
- Gates: every `_trans` case runs and reports a time; no change to engine
  behaviour without a failing test.

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
