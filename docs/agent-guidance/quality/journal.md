# Quality Loop Journal

Append-only log of quality-loop iterations. Newest entries at the bottom.
Entry format: `docs/agent-guidance/skills/quality-loop/SKILL.md` § Journal entry format.
Do not edit or delete past entries; corrections get a new entry.

Grooms archive resolved history from `backlog.md` into this file's commit
trail — the journal plus `git log --grep "quality("` is the full audit record.

---

## Contents

- [2026-07-03 00:00 BOOTSTRAP DONE [opus]](#2026-07-03-0000-bootstrap-done-opus)
- [2026-07-11 20:45 MSR FULL-SCALE RAMP DONE [fable]](#2026-07-11-2045-msr-full-scale-ramp-done-fable)
- [2026-07-11 21:50 VPS HW-COUNTER PROFILING DONE [fable]](#2026-07-11-2150-vps-hw-counter-profiling-done-fable)
- [2026-07-12 12:05 MSR RECEIVER-PROVED BASELINE + PROCESS PERF DONE [codex]](#2026-07-12-1205-msr-receiver-proved-baseline-process-perf-done-codex)
- [2026-07-12 12:15 MSR WORKER-SWEEP PROFILING DONE [codex]](#2026-07-12-1215-msr-worker-sweep-profiling-done-codex)
- [2026-07-12 12:32 MSR 3-WORKER FULL CONFIRMATION REJECTED [codex]](#2026-07-12-1232-msr-3-worker-full-confirmation-rejected-codex)
- [2026-07-12 13:05 MSR DASHBOARD PERF SNAPSHOT AFTER HEALTH FIX [codex]](#2026-07-12-1305-msr-dashboard-perf-snapshot-after-health-fix-codex)
- [2026-07-12 15:18 MSR RTMP OWNERSHIP BENCH + BACKLOG GROOMED [codex]](#2026-07-12-1518-msr-rtmp-ownership-bench-backlog-groomed-codex)
- [2026-07-12 15:36 Q-011 DONE [codex]](#2026-07-12-1536-q-011-done-codex)
- [2026-07-12 15:55 MSR RESTORED DASHBOARD SAMPLE [codex]](#2026-07-12-1555-msr-restored-dashboard-sample-codex)
- [2026-07-12 16:30 Q-013 DONE [codex]](#2026-07-12-1630-q-013-done-codex)
- [2026-07-12 16:45 Q-012 AFFINITY PROBE [codex]](#2026-07-12-1645-q-012-affinity-probe-codex)
- [2026-07-12 17:05 Q-012 CLEAN AFFINITY A/B [codex]](#2026-07-12-1705-q-012-clean-affinity-ab-codex)
- [2026-07-12 17:20 Q-012 RUNTIME AFFINITY PROTOTYPE REJECTED [codex]](#2026-07-12-1720-q-012-runtime-affinity-prototype-rejected-codex)
- [2026-07-12 17:24 Q-012 TOKIO BLOCKING CAP PROBE [codex]](#2026-07-12-1724-q-012-tokio-blocking-cap-probe-codex)
- [2026-07-12 17:32 Q-012 TOKIO THREAD-NAME PROBE [codex]](#2026-07-12-1732-q-012-tokio-thread-name-probe-codex)
- [2026-07-12 17:40 Q-012 TOKIO KEEPALIVE PROTOTYPE REJECTED [codex]](#2026-07-12-1740-q-012-tokio-keepalive-prototype-rejected-codex)
- [2026-07-12 17:45 MSR FULL FINAL PASS [codex]](#2026-07-12-1745-msr-full-final-pass-codex)
- [2026-07-17 22:23 Q-001 STARTED [codex]](#2026-07-17-2223-q-001-started-codex)
- [2026-07-17 22:40 Q-001 DONE [codex]](#2026-07-17-2240-q-001-done-codex)
- [2026-07-17 22:44 Q-017 STARTED [codex]](#2026-07-17-2244-q-017-started-codex)
- [2026-07-17 22:49 Q-017 DONE [codex]](#2026-07-17-2249-q-017-done-codex)
- [2026-07-17 22:55 Q-002 STARTED [codex]](#2026-07-17-2255-q-002-started-codex)
- [2026-07-17 23:18 Q-002 DONE [codex]](#2026-07-17-2318-q-002-done-codex)
- [2026-07-17 23:23 Q-018 STARTED [codex]](#2026-07-17-2323-q-018-started-codex)
- [2026-07-17 23:30 Q-018 DONE [codex]](#2026-07-17-2330-q-018-done-codex)
- [2026-07-17 23:34 Q-019 STARTED [codex]](#2026-07-17-2334-q-019-started-codex)
- [2026-07-18 00:10 Q-019 DONE [codex]](#2026-07-18-0010-q-019-done-codex)
- [2026-07-18 00:15 Q-020 STARTED [codex]](#2026-07-18-0015-q-020-started-codex)
- [2026-07-18 00:45 Q-020 DONE [codex]](#2026-07-18-0045-q-020-done-codex)
- [2026-07-18 00:50 Q-021 STARTED [codex]](#2026-07-18-0050-q-021-started-codex)
- [2026-07-18 01:20 Q-021 DONE [codex]](#2026-07-18-0120-q-021-done-codex)
- [2026-07-18 01:25 Q-022 STARTED [codex]](#2026-07-18-0125-q-022-started-codex)
- [2026-07-18 01:55 Q-022 DONE [codex]](#2026-07-18-0155-q-022-done-codex)
- [2026-07-18 02:00 Q-014 STARTED [codex]](#2026-07-18-0200-q-014-started-codex)
- [2026-07-18 02:20 Q-014 DONE [codex]](#2026-07-18-0220-q-014-done-codex)
- [2026-07-18 02:35 Q-015 STARTED [codex]](#2026-07-18-0235-q-015-started-codex)
- [2026-07-18 03:20 Q-015 DONE [codex]](#2026-07-18-0320-q-015-done-codex)
- [2026-07-18 03:25 Q-016 STARTED [codex]](#2026-07-18-0325-q-016-started-codex)
- [2026-07-18 04:05 Q-016 DONE [codex]](#2026-07-18-0405-q-016-done-codex)
- [2026-07-18 04:10 Q-003 STARTED [codex]](#2026-07-18-0410-q-003-started-codex)
- [2026-07-18 06:15 Q-003 AVIO-FIX DONE [codex]](#2026-07-18-0615-q-003-avio-fix-done-codex)
- [2026-07-18 07:40 Q-004 DONE [codex]](#2026-07-18-0740-q-004-done-codex)
- [2026-07-18 08:10 Q-005 DONE [codex]](#2026-07-18-0810-q-005-done-codex)
- [2026-07-18 08:35 Q-007 DONE [codex]](#2026-07-18-0835-q-007-done-codex)
- [2026-07-18 09:10 Q-006 DONE [codex]](#2026-07-18-0910-q-006-done-codex)
- [2026-07-18 09:40 Q-008 DONE [codex]](#2026-07-18-0940-q-008-done-codex)
- [2026-07-18 10:20 Q-003 DONE [codex]](#2026-07-18-1020-q-003-done-codex)
- [2026-07-18 15:38 AVIO-LOOM DONE [codex]](#2026-07-18-1538-avio-loom-done-codex)
- [2026-07-18 16:20 HUNT RTMP-EGRESS-PERCENT-DECODE DONE [codex]](#2026-07-18-1620-hunt-rtmp-egress-percent-decode-done-codex)
- [2026-07-18 16:40 Q-009 DONE [opus]](#2026-07-18-1640-q-009-done-opus)
- [2026-07-18 16:45 HUNT SRT-MONITOR-OVERFLOW DONE [codex]](#2026-07-18-1645-hunt-srt-monitor-overflow-done-codex)
- [2026-07-18 17:20 HUNT ENGINE-SNAPSHOTS-POISON DONE [codex]](#2026-07-18-1720-hunt-engine-snapshots-poison-done-codex)
- [2026-07-18 17:20 Q-010 DONE [opus]](#2026-07-18-1720-q-010-done-opus)
- [2026-07-18 17:55 Q-012 DONE [opus]](#2026-07-18-1755-q-012-done-opus)
- [2026-07-18 18:05 HUNT MPEGTS-PROBE-AUDIO-BOUNDARY DONE [codex]](#2026-07-18-1805-hunt-mpegts-probe-audio-boundary-done-codex)
- [2026-07-18 20:45 HUNT FFMPEG-STAGE-PLAN-CODEC-DEFAULT DONE [codex]](#2026-07-18-2045-hunt-ffmpeg-stage-plan-codec-default-done-codex)
- [2026-07-18 21:15 HUNT HLS-PREVIEW-GRAPH-CANCEL DONE [codex]](#2026-07-18-2115-hunt-hls-preview-graph-cancel-done-codex)
- [2026-07-18 21:45 HUNT SRT-STREAM-ID-ADVERSARIAL DONE [codex]](#2026-07-18-2145-hunt-srt-stream-id-adversarial-done-codex)
- [2026-07-18 22:05 HUNT STAGE-METRICS-COUNTER-BOUNDARIES DONE [codex]](#2026-07-18-2205-hunt-stage-metrics-counter-boundaries-done-codex)
- [2026-07-18 22:20 HUNT PIPE-METRICS-COUNTER-BOUNDARIES DONE [codex]](#2026-07-18-2220-hunt-pipe-metrics-counter-boundaries-done-codex)
- [2026-07-18 22:40 HUNT ENGINE-HLS-CONSUMER-IDLE-BOUNDARIES DONE [codex]](#2026-07-18-2240-hunt-engine-hls-consumer-idle-boundaries-done-codex)
- [2026-07-18 23:05 HUNT SRT-QUALITY-COUNTER-BOUNDARIES DONE [codex]](#2026-07-18-2305-hunt-srt-quality-counter-boundaries-done-codex)
- [2026-07-18 23:30 HUNT SRT-MUXER-SHARD-POOL-BOUNDARIES DONE [codex]](#2026-07-18-2330-hunt-srt-muxer-shard-pool-boundaries-done-codex)
- [2026-07-18 23:50 HUNT SRT-POLICY-FALLBACK-SEMANTICS DONE [codex]](#2026-07-18-2350-hunt-srt-policy-fallback-semantics-done-codex)
- [2026-07-19 00:10 HUNT TRANSCODE-PROFILE-VALIDATION-BOUNDARIES DONE [codex]](#2026-07-19-0010-hunt-transcode-profile-validation-boundaries-done-codex)
- [2026-07-19 00:35 HUNT API-VIEW-MODELS-FORMATTING-HELPERS DONE [codex]](#2026-07-19-0035-hunt-api-view-models-formatting-helpers-done-codex)
- [2026-07-19 01:00 HUNT RESOURCE-MAP-JSON-SHAPING-HELPERS DONE [codex]](#2026-07-19-0100-hunt-resource-map-json-shaping-helpers-done-codex)
- [2026-07-19 01:20 HUNT STATUS-CPU-AFFINITY-OVERFLOW FIXED [codex]](#2026-07-19-0120-hunt-status-cpu-affinity-overflow-fixed-codex)
- [2026-07-19 02:00 HUNT HLS-PREVIEW-CODEC-LEVEL-DEFAULT FIXED [codex]](#2026-07-19-0200-hunt-hls-preview-codec-level-default-fixed-codex)
- [2026-07-19 02:20 HUNT INGEST-SECURITY-VALIDATE-BRANCHES DONE [codex]](#2026-07-19-0220-hunt-ingest-security-validate-branches-done-codex)
- [2026-07-19 02:35 HUNT SETTINGS-BACKEND-POLICY-FALLBACK DONE [codex]](#2026-07-19-0235-hunt-settings-backend-policy-fallback-done-codex)
- [2026-07-19 02:50 HUNT SRT-INGEST-APPCONFIG-FALLBACK DONE [codex]](#2026-07-19-0250-hunt-srt-ingest-appconfig-fallback-done-codex)
- [2026-07-19 03:05 HUNT RECORDING-SETTINGS-FALLBACK-AND-SHORT-CIRCUIT DONE [codex]](#2026-07-19-0305-hunt-recording-settings-fallback-and-short-circuit-done-codex)
- [2026-07-19 03:30 HUNT INGEST-AUTH-ASYMMETRY-AND-FILE-INGEST-GAPS DONE [codex]](#2026-07-19-0330-hunt-ingest-auth-asymmetry-and-file-ingest-gaps-done-codex)
- [2026-07-19 03:55 HUNT RECONCILE-DECISION-BRANCH-COVERAGE DONE [codex]](#2026-07-19-0355-hunt-reconcile-decision-branch-coverage-done-codex)
- [2026-07-19 04:10 HUNT EGRESS-MALFORMED-URL-RESILIENCE DONE [codex]](#2026-07-19-0410-hunt-egress-malformed-url-resilience-done-codex)
- [2026-07-19 06:33 HUNT SECURITY-EVICTION-BAN-BYPASS FIXED [codex]](#2026-07-19-0633-hunt-security-eviction-ban-bypass-fixed-codex)
- [2026-07-19 HUNT STAGE-LIFECYCLE-STALE-SPAWN-METADATA FIXED [codex]](#2026-07-19-hunt-stage-lifecycle-stale-spawn-metadata-fixed-codex)
- [2026-07-19 HUNT TRANSCODER-SCALE-PATH-PTS-DEFAULT FIXED [codex]](#2026-07-19-hunt-transcoder-scale-path-pts-default-fixed-codex)

## 2026-07-03 00:00 BOOTSTRAP DONE [opus]
- What: quality-loop system created — skills (quality-loop, proof-sweep,
  resilience-sweep, perf-sweep, modularity-sweep, backlog-groom), state files,
  and 10 seed items Q-001…Q-010.
- Gates: n/a (infrastructure only, no engine code touched)
- Commit: (bootstrap session)
- Follow-ups: Q-001…Q-010 filed
- Notes: seeds ground in the 2026-06-27 CPU/RSS profile, the 2026-07-02
  concurrency-proof coverage doc, and docs/layering-roadmap.md. First real
  iterations should prefer Q-003/Q-005/Q-006 (baselines) so later regressions
  are detectable.

## 2026-07-11 20:45 MSR FULL-SCALE RAMP DONE [fable]
- What: first full-scale Mahashivratri msr measurement — smoke (30 outputs)
  then `MSR_FULL=1` ramp 30/120/300/600/900/1,200 on a dedicated Contabo VPS
  (6 vCPU EPYC gen1, 11 GiB RAM; WSL2 dev box was occupied by a live rollout
  run). Sink tuned first: `writeQueueSize: 512` carried from
  test/harness/mediamtx-sink.yml into the msr inline MediaMTX config.
- Gates: msr PASS at all checkpoints; zero warn/error/panic in restream logs
  (~23k lines); fixture-discipline rg scan clean; bench build green on VPS.
- Commit: 6fc2f254 (sink tuning); measurement rows in baselines.md
  § "Mahashivratri msr full-scale ramp — 2026-07-11 (VPS)".
- Follow-ups: MSR-01 link certification and Phase 3 bitrate envelope still
  open; 12h soak at 1,200 on the VPS next (should cross a synthetic 33-bit
  PTS wrap — SR-1); Q-003/Q-005/Q-006 WSL baselines still blocked on the
  live run ending.
- Notes: no capacity knee on 6 cores — 1,200 outputs ≈ 2.4 cores avg,
  447 MB RSS, sublinear CPU scaling. Hero-scenario doc status flipped to
  "measured at full scale (connection-scale phase)".

## 2026-07-11 21:50 VPS HW-COUNTER PROFILING DONE [fable]
- What: profiled the live 1,200-output soak on the VPS with perf + AMD vPMU
  (KVM exposes hardware counters; WSL2 does not). Root-caused the pegged CPU
  core: SRT ingest epoll waiter (`src/media/srt.rs:1536` spawn_blocking loop)
  busy-spins in libsrt `CEPoll::wait` when the socket is continuously
  read-ready — ~1 core per SRT ingest, scale-independent. Also attributed a
  second core to 61 libsrt RcvQ multiplexer threads (one pair per SRT egress).
  Confirmed tokio is not bin-packed (default `worker_threads = num_cpus`,
  no affinity; `RESTREAM_TOKIO_WORKER_THREADS` override exists but unset).
- Gates: n/a (measurement only; no engine code touched; profiling attached
  to the running soak without disturbing it).
- Commit: baselines.md § "Profiling notes (VPS)" (this commit).
- Follow-ups: fix candidate — re-arm handshake or blocking-mode recv for the
  ingest epoll waiter; bin-packing experiment (2–3 workers) informed by the
  hot/cool counter contrast; consider libsrt muxer sharing for SRT egress.
- Notes: hot spinning thread IPC 2.13 / 0.03% L1d miss vs idle scheduler
  worker IPC 0.45 / 8.3% branch miss / 807 migrations/s — strong quantitative
  case that fewer, busier workers win on this workload.

## 2026-07-12 12:05 MSR RECEIVER-PROVED BASELINE + PROCESS PERF DONE [codex]
- What: reran Mahashivratri full MSR on the 6-vCPU EPYC VPS after adding
  harness-side MediaMTX receiver proof. The first full attempt failed at 120
  outputs because `/v3/paths/list` is paginated at 100 items by default; fixed
  the generic MediaMTX probe to walk all pages, then reran the full
  30/120/300/600/900/1,200 ramp successfully. Added a second full run with
  `perf stat -p <restream-pid>` so hardware counters cover the Restream process
  only, not the harness/MediaMTX wrapper.
- Gates: `cargo fmt --all --check`; `cargo clippy --all-targets --all-features
  -- -D warnings`; `cargo test`; `cargo test mediamtx --bin test_harness`;
  `MSR_OUTPUT_COUNTS=120` pagination repro/fix run; `MSR_FULL=1` clean run
  PASS with `1200/1200` MediaMTX paths ready and bytes growing; process-mode
  perf run PASS with `1200/1200` ready.
- Commit: `0e4774e` (pagination fix; earlier same-session commits `85ebdf6`
  SRT plain keys and `5778632` MediaMTX path-health verifier).
- Follow-ups: process-mode perf shows IPC 0.28, cache misses 26.96% of cache
  references, and 10,143 CPU migrations; next optimization should test
  thread/worker bin-packing or affinity before packet-layout changes. VPS 12h
  soak with egress-failure health-latency proof remains open from the original
  launch-hardening checklist.
- Notes: clean full run had zero warn/error/panic lines across Restream,
  MediaMTX, and publisher logs. The process-mode perf run passed but MediaMTX
  emitted one SRT TS decode warning near shutdown/load, so that run is retained
  for counters, not as the log-noise baseline.

## 2026-07-12 12:15 MSR WORKER-SWEEP PROFILING DONE [codex]
- What: ran a short 300-output MSR worker-count sweep with
  `RESTREAM_TOKIO_WORKER_THREADS=2,3,4,6`, process-mode `perf stat -p`, and
  MediaMTX `/v3/paths/list` byte-growth proof at every checkpoint. Added a
  follow-up 3-worker thread census to check whether the SRT thread issue is
  visible before the full 1,200-output shape.
- Gates: all four sweep runs PASS with `300/300` MediaMTX paths ready and bytes
  growing; all four sweep logs had zero warn/error/panic lines; 3-worker thread
  census run PASS with `300/300` ready.
- Commit: (this commit)
- Follow-ups: 3 workers had the best CPU result at 300 outputs, but IPC stayed
  below 0.3 and cache misses stayed above 31% for every worker count, so do not
  change the production default yet. Promote 3 workers to a full 1,200-output
  confirmation run, then prioritize SRT muxer/thread sharing: the census showed
  16 `SRT:RcvQ:*` plus 16 `SRT:SndQ:*` threads at only 15 SRT egresses plus
  ingest, which scales directly into the 60-SRT-output MSR shape.
- Notes: 2 workers passed liveness but was too constrained; 6 workers passed
  but used more CPU/RSS than the 3-worker run at this checkpoint.

## 2026-07-12 12:32 MSR 3-WORKER FULL CONFIRMATION REJECTED [codex]
- What: promoted the 3-worker candidate from the 300-output sweep to a full
  `MSR_FULL=1` ramp with MediaMTX API proof and process-mode `perf stat -p`.
- Gates: full run PASS for receiver liveness at every checkpoint including
  `1200/1200` MediaMTX paths ready and bytes growing.
- Commit: (this commit)
- Follow-ups: do not change the production Tokio worker default from this
  evidence. The full run produced many Restream `sqlx` slow query/pool acquire
  warnings plus MediaMTX SRT TS decode warnings, and final CPU was worse than
  the clean default-worker baseline. Next worker-sizing work should detect the
  effective CPU quota/mask and combine it with workload shape rather than using
  a fixed low worker count.
- Notes: perf counters improved in isolation (IPC 0.31, cache misses 22.35%),
  but the warning profile shows the runtime was under-provisioned for the
  full-scale lifecycle/control-plane burst.

## 2026-07-12 13:05 MSR DASHBOARD PERF SNAPSHOT AFTER HEALTH FIX [codex]
- What: left a full 1,200-output MSR run active on `127.0.0.1:3030` after
  committing `844a7c3` (health snapshots no longer hold coupled registry
  guards). Attached process-mode `perf stat` to the live Restream pid and
  captured a `/proc` thread CPU census without restarting or reshaping the
  dashboard workload.
- Gates: measurement-only while live; no cargo/check/clippy run because
  Restream, MediaMTX, and ffmpeg were intentionally running. Restream
  `/healthz` stayed responsive, and MediaMTX `/v3/paths/list` reported
  `1200/1200` paths ready with bytes growing over a 3-second spot check.
- Commit: (this commit)
- Follow-ups: 12h soak plus live egress-failure health-latency proof remains
  open. Optimization should prioritize effective-CPU/workload-shape worker
  heuristics and SRT muxer/thread sharing before hot/cold member layout work.
  Add one allocator-arena experiment (`MALLOC_ARENA_MAX` or alternate allocator)
  before data-structure field-layout work because RSS growth was private
  anonymous memory while named media buffers stayed flat. If graph refresh is
  promoted from ad hoc inspection to regular operations, add a server-side
  grouped-leaf graph view: the current raw MSR graph is complete but 9.33 MB.
- Notes: current live sample used 4.276 Restream CPUs with IPC 0.37,
  19.93% cache misses, 10.21% branch misses, and 130.755 migrations/sec.
  Thread census again showed six hot Tokio scheduler workers plus roughly one
  low-CPU `SRT:RcvQ:*` thread per SRT socket. RSS rose from 323,884 KiB to
  1,088,120 KiB over the observed 1,200-output window; `/proc` attributed most
  of it to private anonymous mappings, including several nearly-full 64 MiB
  arenas.
  The authenticated health proof split the control-plane cost: full health was
  bounded but heavy (~3.95 MB, p50 392 ms), summary health was fast (~175 KB,
  p50 28 ms), and dashboard runtime summary stayed around p50 362 ms because
  metrics collection performs a synchronous 250 ms network-rate sample. The
  raw pipeline graph endpoint returned the full MSR topology (1,259 nodes,
  1,258 edges) in ~401 ms p50, and the frontend now folds repeated egress
  leaves locally. A later 2-minute observation showed summary health p50 33 ms
  with MediaMTX bytes advancing by 7.09 GB while RSS held flat around 1.09 GB,
  so the memory evidence currently looks like allocator/native growth to a
  plateau rather than an unbounded ring-buffer leak.

## 2026-07-12 15:18 MSR RTMP OWNERSHIP BENCH + BACKLOG GROOMED [codex]
- What: ran the focused `codec/rtmp_payload_ownership` benchmark added in
  `6efa461`, recorded the medians in `baselines.md`, and filed MSR-derived
  backlog items for RTMP payload ownership, CPU affinity/bin-packing, and
  allocator arena limits.
- Gates: `scripts/build/resource-limit.sh cargo bench --bench
  codec_conversions -- 'codec/rtmp_payload_ownership' --warm-up-time 1
  --measurement-time 2 --sample-size 20` passed on an idle host.
- Commit: (this commit)
- Follow-ups: Q-011, Q-012, Q-013.
- Notes: video payload ownership transfer is promising in isolation
  (`-10%` to `-29%` median for 8 KiB to 80 KiB frames), but audio was noise and
  no runtime RTMP change should land without a full before/after MSR receiver
  proof and process counters.

## 2026-07-12 15:36 Q-011 DONE [codex]
- What: tested the RTMP Raw video payload ownership transfer at full MSR scale
  and rejected it; the runtime code was reverted to the reusable-buffer +
  `Bytes::copy_from_slice` path.
- Gates: `scripts/build/resource-limit.sh cargo test rtmp --lib` passed
  (`82` tests); `scripts/build/resource-limit.sh cargo bench --bench
  codec_conversions -- 'codec/rtmp_payload_ownership' --warm-up-time 1
  --measurement-time 2 --sample-size 20` passed; full 1,200-output MSR
  experiment reached `1200/1200` MediaMTX paths ready with bytes growing; 20 s
  `perf stat -p` collected.
- Commit: (this commit)
- Follow-ups: keep Q-012 and Q-013; do not pursue per-packet Vec ownership
  transfer for RTMP video without a different allocator/serializer design.
- Notes: experiment CPU was `2.468` cores vs the current recorded baseline
  `2.304`, cache misses were slightly worse, and page faults rose to
  `60.491/sec`; context-switch/migration reductions were not enough to make it
  a net win.

## 2026-07-12 15:55 MSR RESTORED DASHBOARD SAMPLE [codex]
- What: kept the restored post-Q-011 MSR full dashboard run alive on port 3030
  and collected one short process-mode perf/memory sample without rebuilding or
  disturbing the live stack.
- Gates: paginated MediaMTX `/v3/paths/list` proof before and after the perf
  attach showed `1200/1200` ready paths and bytes growing (`164,832,352` then
  `166,214,003` over 3 s); `/healthz` returned 200.
- Commit: (this commit)
- Follow-ups: Q-012 remains the right place for CPU affinity/effective-mask
  experiments; Q-013 remains the right place for allocator arena/RSS work.
- Notes: restored runtime sample used `2.527` CPUs over 15 s, IPC `0.367`,
  cache misses `18.70%`, context switches `6.118 K/sec`, CPU migrations
  `676.333/sec`, and RSS/PSS `333,840/323,384 KiB`. This artifact has one
  startup slow-SQL warning, so it is a dashboard/perf observation rather than a
  zero-warning certification baseline.

## 2026-07-12 16:30 Q-013 DONE [codex]
- What: tested `MALLOC_ARENA_MAX=2` as a single-variable allocator arena cap
  against a short 1,200-output MSR run and rejected it as a default operator
  setting.
- Gates: MediaMTX `/v3/paths/list` showed `1200/1200` ready with bytes growing
  before perf (`162,241,256` over 3 s), after perf (`169,653,485` over 3 s),
  and after an additional settle window (`183,699,931` over 3 s). Restream logs
  had zero warn/error/panic lines.
- Commit: (this commit)
- Follow-ups: keep allocator arena limits as an emergency deployment knob only;
  do not wire them into runtime defaults without a longer soak and p99 latency
  proof. If memory pressure remains a priority, compare allocators directly
  under the same receiver-proofed MSR method.
- Notes: RSS/PSS improved from `333,840/323,384 KiB` in the restored sample to
  `317,444/299,104 KiB` after settle with `MALLOC_ARENA_MAX=2`, but CPU rose
  from `2.527` to `2.600` cores, CPU migrations rose from `676.333/sec` to
  `712.867/sec`, and page faults rose from `0.067/sec` to `76.467/sec`.
  Hugepages were not active (`AnonHugePages: 0 KiB`); dTLB load misses were
  visible (`12.95%`), but the evidence supports only a later targeted
  large-buffer experiment, not global THP.

## 2026-07-12 16:45 Q-012 AFFINITY PROBE [codex]
- What: ran an external, reversible `taskset` partition probe on the live
  arena-capped MSR run: SRT helper threads pinned to CPUs `0-1`, all other
  Restream threads pinned to CPUs `2-5`, then restored to the original `0-5`
  masks. No runtime code changed.
- Gates: MediaMTX `/v3/paths/list` stayed at `1200/1200` ready with bytes
  growing before and after perf (`190,135,626` then `220,513,390` over 3 s).
- Commit: (this commit)
- Follow-ups: Q-012 remains open. A runtime affinity subsystem needs a clean
  default-runtime A/B, an ownership-aware placement design derived from the
  effective CPU mask, and concurrency gates. The current evidence is not strong
  enough to pin threads by default.
- Notes: the live process had mask `0-5` and `82` threads: `64`
  `tokio-rt-worker`-named runtime/blocking-pool threads, `6` SRT helper
  threads, `10` SQLite workers, one main thread, and one tracing appender. Only
  two Tokio workers were hot in the sample (`43.80%` CPU each), followed by
  shared SRT muxer workers (`SRT:RcvQ:w2` `16.80%`, `SRT:SndQ:w2` `8.60%`).
  Coarse partitioning reduced CPU from `2.600` to `2.458` cores and migrations
  from `712.867/sec` to `553.133/sec`, but worsened IPC (`0.374` to `0.350`),
  cache misses (`18.37%` to `19.00%`), branch misses (`8.50%` to `8.88%`), and
  context switches (`5.495 K/sec` to `6.292 K/sec`).

## 2026-07-12 17:05 Q-012 CLEAN AFFINITY A/B [codex]
- What: reran the external affinity partition probe on a clean default MSR run
  with no allocator cap and no worker override. SRT helper threads were
  temporarily pinned to CPUs `0-1`, all other Restream threads to CPUs `2-5`,
  then restored to the original masks. No runtime code changed.
- Gates: MediaMTX `/v3/paths/list` stayed at `1200/1200` ready with bytes
  growing before and after both perf windows. Restream logs had zero
  warn/error/panic lines.
- Commit: (this commit)
- Follow-ups: Q-012 remains open but is now narrowed to an opt-in,
  ownership-aware runtime affinity design. Any code change must derive
  partitions from the effective CPU mask/cgroup quota and run concurrency proof
  gates because it changes thread lifecycle/placement behavior.
- Notes: default scheduler used `2.321` cores, IPC `0.336`, cache misses
  `20.80%`, context switches `7.663 K/sec`, and migrations `920.333/sec`.
  Partitioning used `2.051` cores, IPC `0.420`, cache misses `16.25%`,
  context switches `4.330 K/sec`, and migrations `288.533/sec`; page faults
  were the one worse counter (`7.200/sec` to `58.733/sec`). The clean thread
  census again showed `82` threads total with two hot Tokio workers and two hot
  shared SRT queue workers, not 64 busy Tokio workers.

## 2026-07-12 17:20 Q-012 RUNTIME AFFINITY PROTOTYPE REJECTED [codex]
- What: implemented a small Linux-only `RESTREAM_THREAD_AFFINITY=partitioned`
  prototype that scanned `/proc/self/task` and applied the same SRT-vs-other
  CPU partition from inside the process; tested it live, then reverted the
  runtime code before commit.
- Gates: unit tests for CPU-list parsing/thread classification passed;
  `scripts/check/concurrency/fast.sh` passed; `cargo clippy --all-targets
  -- -D warnings` passed; live MSR reached `1200/1200` MediaMTX paths with
  bytes growing before and after perf. Thread census proved masks were applied
  as intended.
- Commit: (this commit records the rejected result and systemd guidance; no
  runtime affinity code remains)
- Follow-ups: prefer systemd `CPUAffinity`/NUMA policy for coarse process
  placement. In-process thread-family pinning remains open, but the next design
  must explain why external `taskset` improved CPU/cache/context switches while
  the first scanner did not.
- Notes: the scanner sample used `2.450` and `2.419` cores across two perf
  windows, with cache misses around `20.6-20.9%` and context switches around
  `7.7-8.0 K/sec`; this is not the external partition result (`2.051` cores,
  `16.25%` cache misses, `4.330 K/sec` context switches), so the code was
  correctly rejected.

## 2026-07-12 17:24 Q-012 TOKIO BLOCKING CAP PROBE [codex]
- What: moved resolved Tokio runtime sizing into typed config and surfaced it in
  `/api/v1/engine/health` host settings plus the startup summary, then reran a
  short 1,200-output MSR checkpoint with `RESTREAM_TOKIO_MAX_BLOCKING_THREADS=32`.
- Gates: `cargo fmt --all --check`, `cargo check`, `cargo clippy --all-targets
  -- -D warnings`, and full `cargo test` passed. The first full test attempt hit
  a transient HLS uploader retry assertion; the focused rerun and full rerun
  passed. The MSR checkpoint status was `PASS` with MediaMTX `1200/1200` ready
  and `139.4 MB` aggregate `bytesReceived` growth.
- Commit: (this commit)
- Follow-ups: do not lower the default Tokio blocking cap yet. Health proved
  the child process resolved `workerThreads=2` and `maxBlockingThreads=32`, but
  the live census still reached `85-86` total threads and `66-68`
  `tokio-rt-worker`-named threads. That means the observed 64-ish named threads
  are not explained solely by the main runtime cap; keep investigating Tokio
  worker replacement/dependency internals before changing defaults.
- Notes: compared with the final uncapped 1,200-output baseline
  (`126.87%` avg CPU, `329.5 MB` RSS peak), the cap-32 checkpoint was worse in
  this short run (`135.93%` avg CPU, `421,892 KiB` RSS peak), so it is a
  rejected tuning path for now.

## 2026-07-12 17:32 Q-012 TOKIO THREAD-NAME PROBE [codex]
- What: added a distinct `restream-tokio` thread name for Restream's Tokio
  runtime so future `ps -L`/`top -H` samples can separate Rust runtime threads
  from libsrt, SQLite, tracing, and native helper threads.
- Gates: `cargo fmt --all --check`, focused binary unit test, `cargo check
  --bin restream`, and `cargo clippy --bin restream -- -D warnings` passed.
- Commit: (this commit)
- Follow-ups: a temporary bench-profile probe using the `rs-tokio-*` prefix
  proved the prior `tokio-rt-worker` census belonged to Restream's main runtime,
  not an unrelated dependency runtime. Tokio reused only the two scheduler
  worker identities across many replacement/blocking threads, so the committed
  form is a fixed family label instead of a misleading unique suffix.
- Notes: the short 1,200-output probe passed with MediaMTX `1200/1200` and
  `133.2 MB` byte growth. It reached the same broad shape as the final baseline:
  low-80s total threads, two hot Tokio scheduler workers, SRT queue workers next,
  and many idle Tokio-family threads.

## 2026-07-12 17:40 Q-012 TOKIO KEEPALIVE PROTOTYPE REJECTED [codex]
- What: prototyped a `RESTREAM_TOKIO_THREAD_KEEP_ALIVE_MS` runtime knob and ran
  a short 1,200-output MSR checkpoint with `100 ms` keepalive, then removed the
  runtime code before commit.
- Gates: focused config/health tests and `cargo check --bin restream` passed
  before the live run; bench-profile harness rebuilt successfully; live MSR
  reported `PASS` with MediaMTX `1200/1200` ready and `141.1 MB` aggregate
  `bytesReceived` growth.
- Commit: (this commit records the rejected result; no Tokio keepalive runtime
  code remains)
- Follow-ups: do not add a keepalive knob or lower idle-thread retention as a
  default tuning path. The `100 ms` run did not shrink the Tokio-named thread
  family, which suggests the extra threads are not merely idle blocking workers
  awaiting keepalive expiry.
- Notes: artifact `.local/artifacts/msr-tokio-keepalive100-20260712T153747Z`
  reached `82` total Restream threads and `64` `restream-tokio` threads, while
  CPU/RSS worsened (`146.9%` average CPU, `429 MB` RSS peak) versus the final
  uncapped baseline (`126.87%`, `329.5 MB`).

## 2026-07-12 17:45 MSR FULL FINAL PASS [codex]
- What: ran the full Mahashivratri MSR ramp from committed bench-profile
  binaries after the performance series and systemd placement guidance commit.
- Gates: harness status `PASS`; every checkpoint from 30 through 1,200 outputs
  had MediaMTX `/v3/paths/list` proof with all expected paths ready and
  aggregate bytes growing; Restream and harness logs had zero warn/error/panic
  lines.
- Commit: (this commit)
- Follow-ups: no runtime affinity code is present. Q-012 remains narrowed to a
  future ownership-aware design; systemd placement is the supported operator
  guidance. A true 12h soak remains separate from this non-soak final ramp.
- Notes: final 1,200-output checkpoint was `rtmp:1140,srt:60`, CPU avg/peak
  `126.87%/131.90%`, RSS peak `329.5 MB`, AVIO HWM `3.3 MB`, and MediaMTX
  bytes delta `208,072,764` over 3 s. Independent process-mode perf at 1,200
  outputs measured `2.339` CPUs, IPC `0.307`, cache misses `20.41%`,
  context switches `3.209 K/sec`, and migrations `388.668/sec` while MediaMTX
  remained `1200/1200` with bytes growing before and after the perf window.

## 2026-07-17 22:23 Q-001 STARTED [codex]
- What: establish a fresh per-module coverage map and file focused proof work
  for the weakest media modules before adding adversarial regressions.
- Gates: pending.
- Commit: none.
- Follow-ups: pending coverage evidence.

## 2026-07-17 22:40 Q-001 DONE [codex]
- What: measured the current Rust suite at `5f1c10f4`, separated live proof
  gaps from zero-execution dead abstractions, and filed focused follow-ups.
- Gates: `npm ci` and `npm run build:frontend` repaired the generated-asset
  prerequisite; `scripts/build/resource-limit.sh cargo llvm-cov
  --summary-only` passed 1,396 tests; `cargo fmt --all --check` passed;
  `scripts/build/resource-limit.sh cargo clippy -- -D warnings` passed.
- Commit: this commit.
- Follow-ups: Q-014 through Q-017.
- Notes: the first instrumented run found four API 404s sharing one missing
  generated asset. The canonical frontend build initially failed because the
  copied dependency cache lacked React while the worktree helper reported it
  ready; `npm ci` repaired the environment and the clean rerun passed. Current
  cargo-llvm-cov/Rust instrumentation emitted zero branch records, so the map
  records line coverage and uses function/region coverage as the available
  branch-risk signal instead of inventing branch percentages.

### Top-level `src/` coverage map

| Module | Lines covered | Line coverage |
|---|---:|---:|
| `alerts.rs` | 1,212 / 1,235 | 98.14% |
| `api/` | 4,207 / 5,370 | 78.34% |
| `api_runtime_views/` | 1,623 / 2,011 | 80.71% |
| `api_view_models.rs` | 1,118 / 1,319 | 84.76% |
| `application/` | 5,687 / 7,012 | 81.10% |
| `bin/` | 4,693 / 20,525 | 22.86% |
| `config.rs` | 771 / 780 | 98.85% |
| `db/` | 1,369 / 1,457 | 93.96% |
| `diag.rs` | 432 / 1,010 | 42.77% |
| `domain/` | 1,341 / 1,462 | 91.72% |
| `events.rs` | 293 / 319 | 91.85% |
| `ffmpeg_extract.rs` | 124 / 187 | 66.31% |
| `infrastructure/` | 704 / 745 | 94.50% |
| `lib.rs` | 97 / 731 | 13.27% |
| `logging.rs` | 75 / 274 | 27.37% |
| `main.rs` | 4 / 42 | 9.52% |
| `media/` | 17,902 / 22,099 | 81.01% |
| `planner/` | 611 / 618 | 98.87% |
| `runtime/` | 117 / 119 | 98.32% |
| `runtime_info.rs` | 569 / 600 | 94.83% |
| `secret_display.rs` | 67 / 80 | 83.75% |
| `test_fixtures.rs` | 165 / 180 | 91.67% |

### Risk-ranked weak `src/media/` files

| File | Lines covered | Line coverage | Function coverage | Disposition |
|---|---:|---:|---:|---|
| `ffmpeg/operation.rs` | 0 / 6 | 0.00% | 0.00% | Q-014: bind to a real owner or remove |
| `ffmpeg/operation_compiler.rs` | 0 / 60 | 0.00% | 0.00% | Q-014: same unused layer |
| `srt/crypto.rs` | 13 / 80 | 16.25% | 50.00% | Q-015 adversarial boundary proof |
| `rtmp.rs` | 221 / 1,301 | 16.99% | 52.54% | Q-016 session fault proof |
| `ffmpeg/stage_plan.rs` | 17 / 67 | 25.37% | 60.00% | covered with Q-014 owner decision |
| `rtmp/egress_transport.rs` | 57 / 191 | 29.84% | 40.91% | follow after RTMP session proof |
| `srt_egress.rs` | 273 / 679 | 40.21% | 56.41% | live/FFI-heavy; inventory in Q-002 |
| `srt.rs` | 411 / 992 | 41.43% | 53.33% | live/FFI-heavy; inventory in Q-002 |
| `srt_monitor.rs` | 35 / 80 | 43.75% | 71.43% | lower uncovered-line impact |
| `engine_snapshots.rs` | 107 / 156 | 68.59% | 76.47% | snapshot error branches |
| `mpegts_probe.rs` | 420 / 576 | 72.92% | 89.29% | probe/reporting paths |
- Aggregate workspace coverage: 43,181 / 68,175 lines (63.34%), 4,501 /
  6,928 functions (64.97%), and 59,936 / 97,231 regions (61.64%).

## 2026-07-17 22:44 Q-017 STARTED [codex]
- What: reproduce and permanently reject incomplete copied frontend dependency
  caches before a worktree is reported ready.
- Gates: pending.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-17 22:49 Q-017 DONE [codex]
- What: strengthened worktree frontend readiness to validate the declared npm
  tree and required build entrypoints, with a synthetic stale/corrupt cache
  regression wired into CI.
- Gates: break-it-first regression rejected the old helper because it accepted
  the legacy cache; fixed regression passed; `bash -n` passed; current
  worktree readiness passed; `npm run build:frontend` passed; `npm run
  test:frontend` passed 128 source tests and 57 compiled-bundle smoke tests;
  `scripts/check/source-audit.sh` and `scripts/check/test-hygiene.sh` passed,
  with the latter running 1,397 Rust tests/doctests and finding no noisy output.
- Commit: this commit.
- Follow-ups: none.
- Notes: readiness now checks Vite, TypeScript, Tailwind, HLS, React JSX,
  React DOM, React type declarations, and `npm ls --all` rather than treating a
  three-file legacy subset as proof that a copied cache satisfies the current
  dependency manifest.

## 2026-07-17 22:55 Q-002 STARTED [codex]
- What: inventory every media parse/demux boundary against deterministic
  malformed-input fault coverage before adding new adversarial proofs.
- Gates: none (read-only discovery).
- Commit: none.
- Follow-ups: pending inventory evidence.

## 2026-07-17 23:18 Q-002 DONE [codex]
- What: mapped externally influenced parse/demux entry points in `src/media/`
  against the five required fault classes. `C` means a focused meaningful
  regression exists, `G` means only a generic/no-panic check exists, `Q-nnn`
  is the filed proof gap, and `N/A` means the owner has no such state.
- Gates: read-only inventory; `node scripts/check/docs.mjs` passed.
- Commit: this commit.
- Follow-ups: Q-018 through Q-022.
- Notes: URL, StreamID, JSON-policy, start-time, and HLS segment-name parsers
  are included so "every parse entry point" is explicit, even where the media
  byte fault categories do not apply. FFmpeg/serde/URL-crate parsers behind
  process or library APIs are not reclassified as local byte parsers.

| Entry point / owner | Truncated header | Oversized declared length | Invalid tag/type | Non-monotonic timestamps | Mid-stream parameter change | Disposition |
|---|---|---|---|---|---|---|
| `TsDemuxer::feed` / `feed_slice` / `find_ts_sync` | C: partial chunks | C: corrupt remainder cap | C: false sync scan | N/A | N/A | Covered; generic `demux_corrupt_input_no_panic` is retained only as a multi-shape smoke beside the focused cap/sync tests |
| `TsDemuxer::process_ts_packet` PES/adaptation parsing | G: corrupt packet smoke | Q-021: PES/adaptation bounds | G: bad start code ignored | G: valid-fixture/round-trip monotonic checks | N/A | Q-021 |
| `TsDemuxer::parse_pat` | G: corrupt packet smoke | G: section is slice-bounded | G: wrong table ID ignored | N/A | N/A | Consolidate with Q-018 structural PSI proof rather than add a PAT-only no-panic test |
| `TsDemuxer::parse_pmt` / `parse_stream_descriptors` | G: corrupt packet smoke | Q-018: program/ES descriptor spans | C: unsupported stream types excluded by `StreamKind` mapping | N/A | C: version rebuild, duplicate idempotence, in-flight PES preservation | Q-018 |
| `mpegts_probe::probe_video` / `parse_h264_sps` / `parse_h265_sps` | G: only sub-two-byte H.265 | Q-019: exhausted/overlong Exp-Golomb fields | C: NAL scanners reject missing/wrong types | N/A | N/A: probe freezes only after complete metadata | Q-019 |
| `mpegts_probe::probe_audio` | C: short/invalid ADTS yields incomplete metadata | N/A: fixed seven-byte header | C: wrong sync rejected | N/A | N/A | Covered by probe completeness and ADTS tests |
| `codec::parse_avcc_config` / `avcc_to_annexb_into` | C: short config and trailing NAL | Q-020: maximum SPS/PPS declarations | C: non-VCL/partial parameter sets rejected | N/A | C: inline parameter sets refresh cache without duplication | Q-020 for the remaining shared AVCC contract |
| `rtmp::flv::{parse_flv_video_meta, flv_avcc_config_annexb_parameter_sets, parse_sps_video_info, classify_flv_video_packet, flv_video_composition_time_ms}` | C: empty, one-byte, short AVCC/SPS | Q-020: shared AVCC declaration matrix | C: unknown codec/type fails closed | C: signed 24-bit composition and per-media monotonic guard | C: refreshed sequence header and cache tests | Covered except shared Q-020; randomized SPS no-panic test is backed by deterministic dimension overflow tests |
| `rtmp::flv::parse_flv_audio_meta` | C: empty and one-byte ASC | N/A: fixed ASC fields | C: AAC data vs sequence header and non-AAC codecs | N/A | N/A | Covered |
| `feeder::parse_video_sequence_header` / startup state | C via `parse_avcc_config` | Q-020 via shared AVCC contract | C: unknown audio and non-keyframe startup suppressed | C: `DtsEnforcer`/fixture decode proof | C: late H.264/H.265 parameter-set seeding | Covered except shared Q-020 |
| `hls::fmp4::{parse_avcc_box, parse_h264_sps_avcc_fields}` | Q-020 | Q-020 | C: invalid SPS profile fields return `None` through fallible bit reader | C: sample duration/CTO bounds and property test | N/A: init entry is rebuilt from the selected sequence header | Q-020 |
| `srt_stream_id::{percent_decode, parse_srt_stream_id}` | C: incomplete `%XX` passthrough | N/A | C: common caller shapes and normalization | N/A | N/A | Covered |
| `srt::url::parse_srt_egress_url` / `rtmp::egress_transport::parse_rtmp_url` | C: missing RTMP path; empty optional SRT query fields | N/A | C: RTMP scheme/IPv4/IPv6 and SRT option parsing | N/A | N/A | Covered at parser plus egress validation boundary |
| `srt::config::parse_pipeline_srt_ingest_policy` | C through serde failure and policy fallback tests | N/A | C: malformed/encrypted policy fails closed | N/A | N/A | Covered |
| `file_ingest::parse_start_time_ms` | C: empty/syntax/negative | Q-022: finite/range overflow | C: invalid components rejected | N/A | N/A | Q-022 |
| `hls::fmp4::parse_fmp4_segment_name` | C: wrong prefix/suffix | C: `u64` parse rejects overflow | C: non-segment names rejected | N/A | N/A | Covered; round-trip property test retained |

## 2026-07-17 23:23 Q-018 STARTED [codex]
- What: prove that malformed newer PMT sections cannot consume a version or
  replace a working MPEG-TS stream map before a valid retransmission arrives.
- Gates: pending break-it-first regression, scoped MPEG-TS test, before/after
  demux benchmark, format, clippy, and full test gates.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-17 23:30 Q-018 DONE [codex]
- What: validated the complete PMT stream-loop layout before reading its
  version or mutating stream/PES state. One consolidated regression injects
  both a 4,095-byte `program_info_length` and a 4,095-byte `ES_info_length`
  into tiny PMTs, proves the working version/map survive, and proves the valid
  same-version retransmission is accepted.
- Gates: break-it-first regression failed with `pmt_version` advancing from 0
  to 1; focused PMT suite passed 23 tests; scoped MPEG-TS suite passed 65
  tests; `cargo fmt --all --check`, clippy with warnings denied, and the full
  Rust test/doctest suite passed. The before/after
  `data_path/mpegts_demux_drain` medians were 865.32/889.08 microseconds and
  783.71/747.28 microseconds for take/reuse respectively; Criterion found no
  regression (one unchanged comparison, one improvement, with noted host
  variance/outliers).
- Commit: this commit.
- Follow-ups: none.
- Notes: this remains off the per-packet steady-state path; structural
  validation runs only when a complete PMT section arrives.

## 2026-07-17 23:34 Q-019 STARTED [codex]
- What: make MPEG-TS H.264/H.265 SPS probing reject exhausted bits,
  overlong Exp-Golomb values, unbounded syntax counts, and invalid crop
  arithmetic without panics or partial metadata.
- Gates: pending break-it-first crafted vectors, scoped MPEG-TS tests,
  before/after demux benchmark, format, clippy, and full test gates.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 00:10 Q-019 DONE [codex]
- What: `probe_video` now parses into a scratch clone and only commits to the
  real `VideoMeta` on full SPS-parse success, closing the partial-metadata
  leak. `parse_h264_sps`/`parse_h265_sps`/`skip_scaling_list`/
  `skip_h265_scaling_list_data` return `Option<()>` and propagate failure via
  `?` instead of silently truncating. `BitReader::read_bit`/`read_bits`/`skip`
  return `None` on exhaustion instead of substituting zero; `read_ue` caps the
  leading-zero run at 31 (was 32, which could overflow the `1 << leading_zeros`
  shift) and uses checked arithmetic for the final value; `read_se` uses
  `i32::try_from` instead of a raw cast. Width/height math uses `u64`
  intermediates with `checked_add`/`checked_mul`/`checked_sub` before
  narrowing back to `u32`, rejecting zero or underflowing dimensions.
  `parse_h265_sps` additionally bounds `chroma_format_idc`, bit depths,
  `log2_max_pic_order_cnt`, and the short-term/long-term reference-picture-set
  counts (`MAX_SHORT_TERM_REF_PIC_SETS`/`MAX_DELTA_POCS`/
  `MAX_LONG_TERM_REF_PICS`) so attacker-controlled loop counts can't drive
  unbounded work or out-of-range indexing. Also fixed a real bug found during
  this pass: H.264 scaling-list size selection used `count` (always ≥ 6, so
  the 4x4/16-entry list was never selected) instead of the spec-correct
  per-matrix-index `index`.
- Gates: three crafted adversarial SPS byte vectors (H.264 Exp-Golomb
  overflow, H.265 crop underflow, H.265 unbounded short-term-RPS count) run
  through `probe_video` under `catch_unwind`, asserting no panic and zeroed/
  `None` metadata on failure; new regression test for the scaling-list index
  bug asserts correct `320x240`/`High` profile decode. Scoped
  `cargo test --lib mpegts` passed 66/66. `cargo fmt --all --check` and
  `cargo clippy --all-targets` (warnings denied) were clean. Full
  `cargo test` passed with no failures, panics, or warnings. Benchmarked
  `data_path/mpegts_demux_drain` before/after: an isolated before run
  (patch stashed) measured 1.79-2.52ms across both variants, while two
  independent after runs (patch applied) measured 748µs-1.07ms, consistently
  ~2x faster than the before run. Given the added logic is strictly more
  `Option` checks with no fewer operations, a 2x speedup from this diff is
  not causally plausible; the before run coincided with the lowest measured
  host-available-memory sample (2399MB) and highest load average of the
  three runs, so it is attributed to WSL2 host contention noise rather than
  a real regression. The two after runs agree with each other (867-1072µs
  and 748-813µs, both within the same order of magnitude) which is the more
  reliable signal. No genuine performance regression from this hardening.
- Commit: this commit.
- Follow-ups: none.
- Notes: SPS probing runs only during initial stream-metadata probing, not
  the per-packet steady-state path; benchmarked anyway per the backlog gate.
  Host bench noise makes single before/after comparisons unreliable here —
  future benchmark-gated items on this host should take at least two samples
  per side before concluding a regression.

## 2026-07-18 00:15 Q-020 STARTED [codex]
- What: audit the three independent AVCC (`AVCDecoderConfigurationRecord`)
  parsers — `codec::parse_avcc_config`, `rtmp::flv::
  flv_avcc_config_annexb_parameter_sets`, `hls::fmp4::parse_avcc_box` — for
  fail-closed behavior on truncated input and maximal declared SPS/PPS
  lengths against tiny backing buffers, and add adversarial regression
  coverage.
- Gates: pending scoped avcc/codec/rtmp/hls::fmp4 tests, format, clippy, and
  full test gates.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 00:45 Q-020 DONE [codex]
- What: `codec::parse_avcc_config` was the one parser of the three that did
  not fail closed: on truncation partway through parsing (missing PPS-count
  byte after a valid SPS, or a truncated PPS length/body) it returned
  whatever Annex-B prefix it had already accumulated — e.g. an SPS-only
  result — instead of rejecting the input outright. That partial result is
  cached as `sps_pps_cache` and later prepended verbatim to keyframes, so a
  truncated config would silently produce an incomplete-but-plausible cached
  parameter set instead of an obvious failure. Rewrote it around a new
  private `parse_avcc_sps_pps(data: &[u8]) -> Option<Vec<u8>>` that uses
  `.get()?`-bounds-checked accessors throughout (the same style already used
  by the other two parsers); the public `parse_avcc_config` wrapper now
  falls back to an empty parameter-set vec via `.unwrap_or_default()` on any
  parse failure instead of returning a partial prefix. `rtmp::flv::
  flv_avcc_config_annexb_parameter_sets` and `hls::fmp4::parse_avcc_box`
  were already `Option`/`?`-based and already failed closed on truncation
  and on oversized declared lengths (bounds-checked `.get(pos..pos+len)?`
  never pre-allocates the declared length before validating it fits), so no
  behavior change was needed there — only test coverage. Also replaced a
  weak existing test, `parse_avcc_config_zero_sps_pps`, which used a 7-byte
  fixture that was silently caught by an unrelated `data.len() < 8`
  early-return guard rather than exercising the zero-count loop body itself;
  widened it to 8 bytes so it exercises real loop logic.
- Gates: added adversarial regression tests to all three parsers: SPS parses
  successfully but the PPS-count byte is missing; SPS parses successfully
  but the PPS length/body is truncated; and a maximal declared SPS length
  (`0xFFFF`) paired with a tiny backing buffer. Scoped `cargo test --lib
  avcc` passed 26/26 across all three parsers' tests, `cargo test --lib
  codec::` passed 52/52, `cargo test --lib rtmp` passed 90/90, `cargo test
  --lib hls::fmp4` passed 17/17. `cargo fmt --all --check` and `cargo
  clippy --all-targets` (warnings denied) were clean. Full `cargo test`
  passed with no failures, panics, or warnings.
- Commit: this commit.
- Follow-ups: none.
- Notes: AVCC parsing runs only when a sequence header/config record
  arrives, not per-packet on the steady-state hot path, so no benchmark gate
  applies. The three parsers remain intentionally separate (different
  framing — FLV-prefixed vs. bare AVCC bytes — and different module
  boundaries) rather than consolidated into a shared helper, per AGENTS.md's
  "add abstractions only when they remove real complexity."

## 2026-07-18 00:50 Q-021 STARTED [codex]
- What: prove `TsDemuxer::process_ts_packet` ignores oversized adaptation
  fields and invalid PES header spans without corrupting demuxer state, and
  that `MAX_PES_BUFFER` remains effective under a continuation-packet flood
  while a later valid PES still demuxes. Files: `src/media/mpegts.rs`,
  `src/media/mpegts_tests.rs`.
- Gates: pending scoped `cargo test media::mpegts::tests --lib`, format,
  clippy, full test gates.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 01:20 Q-021 DONE [codex]
- What: traced all three resource-bound concerns in `process_ts_packet`
  (`src/media/mpegts.rs`) by hand before writing any test, per this repo's
  break-it-first convention:
  - Oversized adaptation field: `payload_offset` is checked against
    `TS_PACKET_SIZE` (line ~342) *before* PID dispatch, the continuity-
    counter write, or any PES-accumulator mutation, so a malformed `af_len`
    that overruns the packet already causes the packet to be dropped with
    zero state mutation — not even the continuity counter is touched.
  - Invalid/truncated PES header spans: every header field read
    (`payload[0..3]`, `[4..6]`, `[7]`, `[8]`, `[9..14]`, `[14..19]`) is
    guarded by an explicit `payload.len() >= N` check, and `data_start = 9 +
    pes_header_len` is bounds-checked against `payload.len()` before any
    elementary data is appended. A header with an absurd declared
    `pes_header_len` (e.g. 255 with only 19 bytes available) parses PTS/DTS
    fine but appends zero bytes; `has_timestamp` can end up `true` with an
    empty `buf`, and `flush_pes`'s `buf.is_empty() || !has_timestamp` guard
    resets it without emitting, so no spurious packet reaches output.
  - `MAX_PES_BUFFER`: every append to `stream.pes.buf` (both the PUSI-branch
    and the continuation-branch) is guarded by `buf.len() + new.len() <=
    MAX_PES_BUFFER`, so the buffer plateaus within one TS-payload-size of the
    cap and can never exceed it, regardless of how many further continuation
    packets arrive.
  All three held in all three cases: this was tests-only, no production code
  changed (confirmed via `git status` after the gate run below — only
  `src/media/mpegts_tests.rs` and `backlog.md` are modified). Added three
  hand-crafted-packet regression tests calling the private
  `TsDemuxer::process_ts_packet` directly (the test submodule already has
  private-item access, matching the existing `try_build_probe_*` tests'
  pattern of pre-seeding `demuxer.streams`/`pid_to_stream` by hand):
  `process_ts_packet_ignores_oversized_adaptation_field_without_state_corruption`,
  `process_ts_packet_rejects_pes_header_len_overrunning_payload`, and
  `process_ts_packet_caps_pes_buffer_at_max_size_under_continuation_flood`
  (the last floods ~6000 continuation packets, well past the ~2849 needed to
  reach the 512 KiB cap, then proves a subsequent legitimate PES on the same
  PID still demuxes correctly — both the capped-and-flushed prior PES and
  the fresh one land as separate correct `MediaPacket`s). While constructing
  the hand-crafted "recovery" PES packets, hit one test-fixture bug worth
  noting for future adversarial packet-crafting: a single-TS-packet PES with
  `pes_packet_len = 0` (unbounded, the standard video encoding) slurps the
  *entire* remaining 184-byte TS payload region into the PES buffer,
  including trailing 0xFF stuffing bytes past the real elementary data —
  `pes_packet_len` is what bounds a PES's true length, not TS-packet
  boundaries. Fixed by giving the "known-good recovery packet" test helper
  an explicit non-zero `pes_packet_len` so `expected_payload_len` truncates
  the stuffing away, and adding a second, separately-named unbounded-start
  helper only for the buffer-flood test where unbounded framing is the
  actual scenario under test.
- Gates: `cargo test media::mpegts::tests --lib` passed 69/69 (66 pre-
  existing + 3 new). `cargo fmt --all --check` and `cargo clippy
  --all-targets` (warnings denied) were clean. Full `cargo test` passed with
  no failures, panics, or warnings across all 18 test binaries + doctests.
  No MPEG-TS demux/resync benchmark run, since no production code changed
  (bench gate is conditional on production-code changes per the backlog
  item).
- Commit: this commit.
- Follow-ups: none.
- Notes: this is the second consecutive backlog item (after Q-020) where the
  adversarial trace found the production code already correct and the real
  gap was missing regression coverage — worth periodically checking whether
  the backlog's remaining `[resilience]` items are converging on "prove
  existing safety" rather than "fix a live bug," which would argue for
  weighting future grooming toward other categories.

## 2026-07-18 01:25 Q-022 STARTED [codex]
- What: harden `file_ingest::parse_start_time_ms` (`src/media/file_ingest.rs`)
  against `NaN`/infinity inputs, float-to-millisecond overflow, and integer
  overflow in the colon-delimited hours/minutes scaling arithmetic.
- Gates: pending scoped `cargo test media::file_ingest::tests --lib`,
  format, clippy, full test gates.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 01:55 Q-022 DONE [codex]
- What: unlike Q-020/Q-021, this trace found a real live bug, not just a
  coverage gap. In `parse_start_time_ms`, the plain-seconds branch parsed
  `trimmed` with `str::parse::<f64>()`, which happily accepts `"nan"`,
  `"inf"`/`"infinity"`, and `"-inf"` (Rust's `f64` `FromStr` grammar).
  `NaN < 0.0` is always `false`, so a `"nan"` input skipped the
  non-negative check entirely, then `(seconds * 1000.0).round() as i64`
  silently produced `0` (Rust's saturating float-to-int cast maps `NaN` to
  `0`) instead of an error — exactly the "coercing to zero" failure mode
  named in the backlog goal. `"inf"` passed the non-negative check (positive
  infinity is not `< 0.0`) and silently saturated to `i64::MAX` instead of
  erroring — the "saturated timestamps" failure mode. The same `seconds`
  parse in the colon-delimited branch had the identical gap. Separately, the
  colon-delimited branch computed `hours * 3600 + minutes * 60` as plain
  `i64` arithmetic with no overflow guard: `hours`/`minutes` are only
  bounds-checked for being non-negative and for fitting in `i64` at parse
  time, so a value like `hours = i64::MAX` parses fine but `hours * 3600`
  overflows — a debug-build panic (arithmetic overflow checks are on) or a
  silently wrapped/garbage value in release.
  Fix (production code, `src/media/file_ingest.rs`): added a
  `seconds_to_ms(seconds: f64) -> Result<Option<i64>, String>` helper that
  rejects non-finite or out-of-`i64`-range millisecond values instead of
  casting through them; both parse branches now call `seconds.is_finite()`
  before the existing non-negative check, and reuse `seconds_to_ms` for the
  final scale/round/cast; the colon-delimited branch replaced raw `hours *
  3600 + minutes * 60` with `checked_mul`/`checked_add`, erroring on
  overflow instead of panicking or wrapping. Added six adversarial
  regression tests to `src/media/file_ingest.rs`'s existing `mod tests`,
  next to the pre-existing `rejects_invalid_start_time` test:
  `rejects_non_finite_plain_seconds` (`"NaN"`, `"nan"`, `"inf"`,
  `"infinity"`, `"-inf"`), `rejects_non_finite_colon_delimited_seconds_component`
  (`"00:nan"`, `"00:00:inf"`), `rejects_float_to_millisecond_overflow`
  (`"1e30"` in both plain and colon-delimited form), and
  `rejects_colon_delimited_integer_overflow` (component values of
  `i64::MAX` in the hours and/or minutes position, each individually
  parseable but overflowing once scaled by 3600/60).
- Gates: `cargo test media::file_ingest::tests --lib` passed 22/22 (16 pre-
  existing + 6 new). `cargo fmt --all --check` and `cargo clippy
  --all-targets` (warnings denied) were clean. Full `cargo test` passed with
  no failures, panics, or warnings across all 18 test-result blocks
  (binaries + doctests). No hot-path benchmark applies —
  `parse_start_time_ms` runs once per file-ingest start, off the packet hot
  path.
- Commit: this commit.
- Follow-ups: none.
- Notes: the `seconds_to_ms` extraction was the smallest correct
  abstraction available — both parse branches (plain-seconds and
  colon-delimited) need identical finite/range validation on their final
  millisecond value, and duplicating the three-way `is_finite`/`>
  i64::MAX`/`< i64::MIN` check inline in both branches would have been the
  kind of copy-paste AGENTS.md's "add abstractions only when they remove
  real complexity" argues against.

## 2026-07-18 02:00 Q-014 STARTED [codex]
- What: decide whether `src/media/ffmpeg/operation.rs` and
  `src/media/ffmpeg/operation_compiler.rs` become part of an actual
  backend-owned execution path or are retired as unused indirection.
- Gates: pending focused backend/stage-runtime tests, format, clippy, full
  test gates, `node scripts/check/docs.mjs`.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 02:20 Q-014 DONE [codex]
- What: re-confirmed Q-001's measurement (0/6 and 0/60 covered lines) by
  grepping `src/`, `test/`, and `benches/` for every symbol the two files
  export (`compile_operation`, `FfmpegOperation`, `VideoEncoderSettings`,
  `AudioOperation`, `VideoCodec`, `ffmpeg::operation::`,
  `operation_compiler::`) — zero consumers anywhere outside the two files
  themselves and the two now-removed `pub mod` declarations in
  `src/media/ffmpeg/mod.rs`. Also checked the architectural claim the module
  doc comments made ("Both the external-process and in-process backends
  consume the same `FfmpegOperation`"): `src/media/ffmpeg/backend.rs`'s
  trait methods take `plan: FfmpegStagePlan`, not `FfmpegOperation`, and
  `src/media/transcoder.rs`/`src/media/external_transcoder.rs` both consume
  `FfmpegStagePlan` directly. `docs/stage-boundary-proof-map.md`'s Input
  pump -> backend row repeated the same false claim ("Shared
  operation/compiler tests" as current proof), which doesn't exist —
  `compile_operation` has no test that calls it because nothing calls it.
  Retired rather than bound: the backlog goal explicitly said "Do not add
  incidental tests for code that no production path consumes," and binding
  would mean inventing a new integration point with no existing motivation
  — `FfmpegStagePlan` is already the real backend-neutral contract and is
  already proven at that boundary.
  Fix (production code): deleted `src/media/ffmpeg/operation.rs` and
  `src/media/ffmpeg/operation_compiler.rs`; removed their `pub mod` lines
  from `src/media/ffmpeg/mod.rs`. Corrected
  `docs/stage-boundary-proof-map.md`'s Input pump -> backend row to
  describe the real current proof: `build_ffmpeg_stage_plan` unit tests in
  `src/media/stage_runtime.rs` (including
  `external_and_internal_stage_plan_share_operation`, which proves one
  `FfmpegStagePlan` is constructed per `StageKind` and carries startup
  policy for both backends) plus `tests/transcoder.rs` integration coverage
  proving the external path (`build_stage_ffmpeg_args`) and internal path
  (`run_ffmpeg_transcode_with_scale`) each produce correct output from that
  plan.
- Gates: `cargo test --lib media::ffmpeg` passed 15/15;
  `cargo test --lib stage_runtime` passed 9/9 (including
  `external_and_internal_stage_plan_share_operation`, unaffected by the
  deletion since it only exercises `build_ffmpeg_stage_plan`). `cargo fmt
  --all --check` and `cargo clippy --all-targets` (warnings denied) were
  clean — no dangling references to the deleted module anywhere in the
  tree. `node scripts/check/docs.mjs` passed (68 files). Full `cargo test`
  passed with no failures. No hot-path benchmark applies — this removed
  dead compile-time indirection, not runtime behavior.
- Commit: this commit.
- Follow-ups: none.
- Notes: this is a retirement, not a fix — no behavior changed, since
  nothing executed this code. The value is removing a doc/comment claim
  that actively misdescribed the architecture (useful for the next agent
  who might otherwise have "fixed" `compile_operation` instead of noticing
  it was never called).

## 2026-07-18 02:35 Q-015 STARTED [codex]
- What: deterministic mutation-proven coverage for `src/media/srt/crypto.rs`
  (plaintext vs. encrypted resolution, URL default key length, every
  supported key length, interior-NUL passphrases, and FFI option failures
  through the existing error surface), per the backlog goal.
- Gates: pending `cargo test srt_crypto --lib`, `cargo fmt --all --check`,
  clippy, concurrency contract gate (srt_egress.rs is lifecycle-adjacent to
  srt.rs), full `cargo test`.
- Commit: none.
- Follow-ups: none yet.

## 2026-07-18 03:20 Q-015 DONE [codex]
- What: added unit coverage for `srt_crypto_from_resolved`,
  `srt_crypto_from_url` (empty-passphrase, default/explicit pbkeylen,
  unvalidated pass-through), and interior-NUL passphrase rejection in
  `apply_srt_crypto_socket`, plus FFI-boundary tests against the real linked
  libsrt for every supported `SRTO_PBKEYLEN` value (16/24/32) and rejection
  of an out-of-range value, run against a real socket via `srt_setsockopt`.
  Bug found while writing the config-object variant of the FFI boundary
  test: `srt_create_config`/`srt_config_add` (libsrt's per-member bonding
  config mechanism) unconditionally rejects `SRTO_PASSPHRASE`,
  `SRTO_PBKEYLEN`, and `SRTO_STREAMID` (see `SRT_SocketOptionObject::add` in
  vendored `socketconfig.cpp`, which has no case for any of the three and
  falls through to `return false`), and `srt_config_add`'s failure path
  never calls `CUDT::APIError`, so `check_srt_option_result` misreported the
  failure as `"failed to set SRTO_PASSPHRASE: Success (0)"` instead of the
  real cause.
  Fix (production code, real bug, not just a test gap): bonded SRT egress
  (`src/media/srt_egress.rs`, `use_bonding` branch) was smuggling passphrase
  and StreamID through this per-member config object, so any bonded egress
  target configured with a passphrase or a non-empty StreamID always failed
  to connect. Rewrote the branch to apply crypto via
  `apply_srt_crypto_socket` and StreamID via `srt_setsockopt` directly on
  the group socket (both group-wide settings in libsrt bonding) before
  `srt_connect_group`, matching the pattern the non-bonded path already
  used correctly. Removed the now-dead `srt_prepare_endpoint`
  member-config-assignment and its `srt_delete_config` cleanup path.
  Retired `apply_srt_crypto_config` (`src/media/srt/crypto.rs`) entirely —
  it has no production caller left, and it can never succeed for
  passphrase/pbkeylen given the libsrt limitation above, so keeping it
  around as a usable-looking API would be actively misleading. Kept the raw
  `srt_create_config`/`srt_config_add`/`srt_delete_config` FFI declarations
  in `srt.rs` since they remain valid for the per-member options libsrt
  *does* allow (e.g. `SRTO_RCVBUF`/`SRTO_SNDBUF`/`SRTO_CONNTIMEO`), even
  though nothing currently calls them.
  Replaced a weak test,
  `bonded_egress_member_config_is_created_for_crypto_without_streamid`,
  which asserted literal source-text substrings of the old (now-rewritten)
  bonded branch — it would have silently kept "passing" against any
  refactor that preserved the exact strings regardless of real behavior,
  and broke outright against this rewrite. In its place: two permanent FFI
  regression tests grounded in real libsrt calls,
  `linked_libsrt_group_socket_accepts_crypto_via_setsockopt` and
  `linked_libsrt_group_socket_accepts_streamid_via_setsockopt`, proving the
  exact mechanism the fixed production code now relies on; and
  `linked_libsrt_member_config_rejects_passphrase_and_streamid`, a tripwire
  documenting the libsrt limitation directly (via `srt_config_add`) so a
  future libsrt upgrade that starts accepting these options — or a future
  regression that reintroduces the broken per-member-config pattern — has a
  test that reacts either way.
- Gates: `cargo test --lib media::srt` passed 91/91 (0 failed). `cargo fmt
  --all --check` clean. `cargo clippy --lib --tests` (warnings denied)
  clean. `scripts/check/concurrency/contract.sh` passed. Full `cargo test`
  passed with no failures (unit + integration + doctests). No hot-path
  benchmark applies — the changed code is the connect-setup path (runs once
  per egress connection, off the packet hot path), not a per-packet loop.
- Commit: this commit.
- Follow-ups: none. Q-016 (RTMP session fault transitions) is next per the
  backlog's proof-tier ordering.
- Notes: started as a proof-coverage item and surfaced a real 100%
  connection-failure bug for bonded SRT egress with a passphrase or
  non-empty StreamID configured — worth flagging prominently for anyone
  grading this item, since the original framing undersold what was found.

## 2026-07-18 03:25 Q-016 STARTED [codex]

- Claimed Q-016 (prove RTMP session fault transitions) from the backlog.
  Goal: the smallest deterministic component proof for malformed or
  truncated RTMP session input, asserting both the surfaced protocol error
  and complete session/registration cleanup, reusing the existing session
  harness rather than a duplicate live pipeline.
- Ruled out FLV-payload parsing as the target: `src/media/rtmp/flv.rs`'s
  parsers are already defensively bounds-checked against malformed input
  and covered by existing tests. Root-caused a deterministic single-byte
  RTMP chunk-protocol fault instead, via the vendored `rml_rtmp` 0.8.0
  source: a chunk basic header byte with a non-zero format on a chunk
  stream id that has never received a type-0 header
  (`ChunkDeserializationError::NoPreviousChunkOnStream`) is invalid per the
  RTMP chunk spec and triggers on the very next read with no further bytes
  needed — byte `0x45` (format `01`, csid `5`).

## 2026-07-18 04:05 Q-016 DONE [codex]

- Added two proofs to `src/media/rtmp/tests.rs`, both driving a real
  `handle_rtmp_client` over a loopback `TcpListener`/`TcpStream` pair with a
  real `rml_rtmp` `ClientSession` publish handshake (via a new
  `drive_client_publish_handshake` helper) and a minimal
  `AcceptAllAuthenticator` test-double for `PipelineAccessAuthenticator`
  (no existing test-double existed for this trait):
  `malformed_chunk_after_publish_surfaces_error_and_clears_ingest_registration`
  (writes the single malformed chunk-header byte after a successful
  publish) and
  `truncated_chunk_then_disconnect_clears_ingest_registration_without_error`
  (writes a lone valid type-0 basic-header byte, then closes the socket
  mid-message, proving a partial chunk plus EOF is treated as an ordinary
  disconnect, not a fault). Both assert on `engine.ingests.active` before
  and after the fault, using the same idiom production code itself uses for
  active-ingest membership.
  Bug found (production code, not just a test gap): in
  `handle_rtmp_client`'s main `tokio::select!` loop
  (`src/media/rtmp.rs`), the `socket.read` arm used
  `.map_err(|_| "...")?` on both the raw read result and
  `session.handle_input(...)`. Both are early `?`-returns out of the whole
  function, which skip the post-loop ingest-cleanup block entirely (that
  block only runs when the loop exits via `break Some(...)`). Concretely:
  any malformed RTMP chunk data arriving after a publisher had already
  registered left that registration in `engine.ingests.active` forever —
  a real ingest-slot leak on nothing more than one crafted byte from an
  already-connected publisher, discoverable by the
  `malformed_chunk_after_publish_...` test above before the fix (it failed
  with the registration still present).
  Fix: converted both `?`-early-returns in the `socket.read` arm to the
  same `warn!` + `break Some((phase, reason, had_error=true))` pattern the
  adjacent `handle_session_results` error arm already used, so every
  mid-loop fault after registration now runs through the single existing
  post-loop cleanup block. The read-error case also gets a new `"io"` phase
  tag so it's distinguishable from a session-parse fault in disconnect
  telemetry. No behavior change for the two paths that were already
  correct: the pre-loop "leftover handshake bytes" error path (had its own
  inline cleanup already) and the clean-EOF (`n == 0`) path.
- Gates: `scripts/build/resource-limit.sh cargo test rtmp --lib` passed
  92/92 (0 failed; includes the two new tests, both failing pre-fix and
  passing post-fix). `cargo fmt --all --check` clean. `scripts/build/resource-limit.sh
  cargo clippy --all-targets` (warnings denied) clean. Full
  `scripts/build/resource-limit.sh cargo test` passed with no failures
  (1074 lib tests + all integration suites + doctests). No hot-path
  benchmark applies — the changed code is per-connection control flow in
  the RTMP session loop's fault paths, not a per-packet loop.
- Commit: this commit.
- Follow-ups: none identified for RTMP session fault handling specifically.
  Coverage for `src/media/rtmp.rs`'s assembled session path remains an area
  future proof-tier backlog items could keep extending (e.g. faults during
  the pre-registration connect/publish-request phase), but the specific gap
  the backlog item named — malformed input leaking registrations after
  publish — is now closed and regression-tested.
- Notes: same pattern as Q-015 — a proof/coverage task surfaced a real
  production bug (a resource leak, not a crash) that the adversarial sweep
  goal explicitly asks to fix and regression-test in place.

## 2026-07-18 04:10 Q-003 STARTED [codex]
- What: seed the Criterion benchmark baseline ledger in `baselines.md` with
  medians for `ring_buffer`, `avio_throughput`, and
  `high_performance_data_path`, each from three clean serial `cargo bench`
  runs on an idle host.
- Gates: pending — three serial `scripts/build/resource-limit.sh cargo
  bench --bench <name>` runs per suite; host confirmed idle via
  `pgrep -x restream; pgrep -x mediamtx; pgrep -x ffmpeg` (all empty)
  immediately before starting.
- Commit: none.
- Follow-ups: pending bench results.

## 2026-07-18 06:15 Q-003 AVIO-FIX DONE [codex]
- What: the first `avio_throughput` bench run under Q-003 hung indefinitely
  at 0% CPU on its first warmup iteration. Root-caused to a lost-wakeup race
  in `MemoryQueue::write`/`write_cancellable`/`write_batch`
  (`src/media/avio.rs`): each called `self.space_available.notified()` (a
  fresh `Notified` future, which only observes notifications from the moment
  it's created) *after* releasing the lock that guarded the capacity check.
  A reader on another OS thread could drain the buffer and call
  `notify_waiters()` in that gap, before the writer's future existed to
  observe it — losing the wakeup and hanging the writer forever. Fixed by
  arming `notified()` before the capacity check in all three write paths, so
  it snapshots the notify generation before the lock is dropped.
- Gates: new regression test
  `write_wakeup_survives_lock_release_race` (multi-thread runtime writer +
  tight-loop OS-thread reader, capacity crossed on nearly every write, 10s
  timeout) fails (hangs to timeout) against pre-fix code and passes
  post-fix. Full `media::avio` module: 19/19 passed, 0 failed. `cargo fmt
  --all --check` clean. `scripts/check/concurrency/contract.sh` passed in
  full (loom suites, proptests, live-harness fault/recovery scenarios, API
  tests) with 0 failures across all logs in
  `.local/artifacts/concurrency-contract-logs/`. `benches/avio_throughput.rs`
  now completes normally instead of hanging.
- Commit: this commit.
- Follow-ups: none identified — the same lost-wakeup shape does not appear
  elsewhere in `avio.rs`'s notify usage (reader-side and close-side
  `notify_waiters()` callers don't hold a matching pre-armed `Notified`
  future because they're not blocking-wait loops). Q-003 baseline-ledger
  seeding resumes now that the bench suite runs cleanly.
- Notes: same pattern as Q-015/Q-016 — a proof/benchmark task surfaced a
  real production bug (a hang, not a crash) that the adversarial sweep goal
  explicitly asks to fix and regression-test in place.

## 2026-07-18 07:40 Q-004 DONE [codex]
- What: classified every `.unwrap()`/`.expect(`/`panic!`/`unreachable!` in
  non-test `src/media/` code as invariant-safe or fallible (delegated the
  line-by-line grep classification to a subagent to keep main-context
  tokens bounded). Confirmed invariant-safe: `file_ingest.rs:791` (guarded
  five lines above by an `if anchor.is_none() { return; }` check),
  `hls/fmp4.rs:659,797` (`Fmp4SegmentMuxer::new().expect(...)` — a
  no-argument constructor; failure would be a static library fault, not
  data-triggerable). Found one fallible panic site:
  `src/media/hls/preview.rs:21`, `get_hls_preview_cancel_token(...).await
  .unwrap()` inside `ensure_hls_preview_runtime`. It re-acquired the
  `hls.consumers` lock after `ensure_hls_preview_segmenter` had already
  dropped it, so a concurrent `shutdown_hls_preview_segmenter` call (from
  the idle-timeout reconciler in `src/lib.rs` or the pipeline-delete
  handler in `src/api/pipelines.rs`) could remove the just-inserted
  consumer entry in that window and turn the `unwrap()` into a live panic.
  Per the adversarial-sweep goal ("fix every failing test... add permanent
  regression tests for each bug found"), fixed in place rather than just
  filing a follow-up: `ensure_hls_preview_segmenter`
  (`src/media/engine_hls.rs`) now returns the cancel token it just
  inserted (or the pre-existing one) directly from the same lock
  acquisition, eliminating the re-read and the race window structurally.
  The structurally identical non-preview pair (`ensure_hls_segmenter` /
  `get_hls_cancel_token`) was left as-is: its one caller
  (`src/lib.rs:818-831`) already handles the `None` case gracefully with a
  `warn!` and early return instead of unwrapping, so it has no reachable
  panic site — no fix needed there.
- Gates: new regression test
  `ensure_hls_preview_runtime_survives_concurrent_shutdown_race`
  (`src/media/hls/preview.rs`) spawns 64 interleaved
  `ensure_hls_preview_runtime` / `shutdown_hls_preview_segmenter` tasks and
  asserts every task joins without panicking. Full `scripts/build/
  resource-limit.sh cargo test --profile bench --lib`: 1091/1091 passed, 0
  failed. `cargo fmt --all --check` clean. `scripts/build/resource-limit.sh
  cargo clippy --profile bench --all-targets` clean (warnings denied).
  `scripts/check/concurrency/fast.sh` passed (135/135). No hot-path
  benchmark applies — `ensure_hls_preview_segmenter` runs once per preview
  session start, not per packet.
- Commit: `f0aec2fe`.
- Follow-ups: none identified. The non-preview `ensure_hls_segmenter` /
  `get_hls_cancel_token` pair shares the same two-call lock shape but has
  no unwrap-on-None caller today; if a future caller adds one without
  checking `None`, it would reintroduce this exact bug shape.
- Notes: same pattern as Q-003/Q-015/Q-016 — a proof-tier inventory task
  surfaced a real concurrency bug (a live panic under a lifecycle race,
  not just a coverage gap) that the adversarial sweep goal explicitly asks
  to fix and regression-test in place, rather than just filing it as a
  separate follow-up item.

## 2026-07-18 08:10 Q-005 DONE [codex]
- What: baselined all four live resilience-contract harness fault modes
  serially on an idle host (`pgrep -x restream; pgrep -x mediamtx; pgrep -x
  ffmpeg` confirmed empty before the run), built once via
  `scripts/build/resource-limit.sh cargo build --profile bench --bin
  test_harness` and run as `target/release/test_harness <mode>` per the
  documented `bench`-profile-output quirk (AGENTS.md: `--profile bench`
  populates `target/release/`, not `target/bench/`, unless run through
  `scripts/build/bench-harness.sh`). Results, parsed from each mode's
  `.local/artifacts/latest/<mode>.json`:
  - `fault.resilience`: `passed: true`, 17/17 sub-tests passed.
  - `fault.egress-retry`: `passed: true`, 4/4 sub-tests passed.
  - `fault.output-stall`: `passed: true`, 2/2 sub-tests passed
    (`rtmp-egress-sink-stalls`, `rtmp-stalled-sink-isolation-under-many-
    outputs` — the latter's raw diagnostic snippet shows `"status":
    "stalled"`, which is the asserted expected state of the isolated sink
    under test, not a failure; its own `passed` field is `true`).
  - `recovery`: `passed: true`, 7/7 sub-tests passed (`transient-rtmp-drop-
    preserves-egress`, `transient-srt-drop-preserves-egress`, `rapid-srt-
    replacement-preserves-egress`, `egress-retry-survives-transient-
    ingest-gap`, `hls-put-timeout-recovers-after-restart`, `rtmp-sink-
    flaps-surface-output-instability`, `srt-sink-flaps-surface-output-
    instability`).
  No failures or flakes found across any of the 30 total sub-tests in this
  baseline run — nothing to file as a follow-up item per the goal's "any
  failure or flake filed as its own item" clause.
- Gates: the four `test_harness` fault/recovery modes themselves are the
  gate for this item (measurement-only task, no source files modified).
- Commit: this commit (journal + backlog doc update only).
- Follow-ups: none — clean baseline across all four modes.
- Notes: unlike Q-003/Q-004/Q-015/Q-016, this proof/measurement pass did
  not surface a bug — the live resilience contract is green. Recorded here
  as the known-good baseline the loop can now diff future runs against to
  detect regressions.

## 2026-07-18 08:35 Q-007 DONE [codex]
- What: mapped every "Contract to prove" row in
  `docs/stage-boundary-proof-map.md` (10 boundary rows) against the
  enumerated checks in `scripts/check/concurrency/fast.sh` and
  `scripts/check/concurrency/contract.sh` (both source the same
  `run_common_concurrency_checks` helper in
  `scripts/check/concurrency/common.sh`, which contract.sh supersets with
  `history-grouping.sh`, `process-lifecycle-guards`, and the four live
  `fault.*`/`recovery` harness modes). Two rows explicitly claim
  mandatory-gate status in their own text: "Runtime admission -> registry"
  (transcoder/TS-muxer replacement-race loom models) and "Cancel/teardown
  -> observable cleanup" (which literally names `fast.sh` as the gate that
  keeps its loom models and status/recovery contracts mandatory). Verified
  both by name: `ts_muxer_stage_loom` and `transcoder_stage_loom` are two
  of the five loom targets `common.sh` loops over
  (`avio_loom, ring_migration_loom, ts_chunk_ring_loom, ts_muxer_stage_loom,
  transcoder_stage_loom`), and the cancel/teardown row's status/recovery
  contracts match `common.sh`'s `api-health`, `api-disconnect-*`,
  `api-egress-*`, and `output-status-*` named test filters plus
  `contract.sh`'s `fault.resilience`/`recovery` harness-mode runs. The
  remaining 8 rows (planner->stage runtime, source ring->input pump, input
  pump->backend, backend->normalizer, audio router, HLS segmenter,
  recording writer, runtime snapshot->status) document proof that lives in
  the general `--lib`/integration test suite rather than the concurrency
  gate scripts — consistent with the Inner Loop routing table in
  `AGENTS.md`, which reserves `fast.sh`/`contract.sh` specifically for
  concurrency primitives, thread hops, and a named list of lifecycle files
  (`engine.rs`, `srt.rs`, `ts_chunk_ring.rs`, `avio.rs`, `recording.rs`,
  `file_ingest.rs`, `external_transcoder.rs`) and routes ordinary module
  changes to a scoped `cargo test <module>` instead. Spot-checked that the
  named tests these 8 rows cite actually exist and match by grepping for
  representative names (`prop_source_stage_chunked_input_preserves_per_
  stream_dts_order` in `tests/transcoder.rs`;
  `loom_publish_model_never_exposes_segment_without_init` in
  `src/media/hls/fmp4.rs`, a self-contained `loom::model` proof that runs
  on every plain `cargo test --lib` since it models its own `ModelState`
  rather than swapping production sync primitives, so it does not need the
  `--cfg loom` gate the five `tests/*_loom.rs` targets use; recording
  identity tests `media_recording_identity_uses_recording_id` /
  `_rejects_metadata_less_filename_fallback` in
  `src/bin/test_harness/mixed_playback.rs`, matching the `cargo test
  media_recording_identity --bin test_harness` command cited in
  `docs/regression-artifacts.md` by substring filter). No rule was found
  claiming mandatory-gate coverage without actually having it, and no
  cited test name was stale or missing.
- Gates: none (grooming task; read-only analysis).
- Commit: this commit (journal + backlog doc update only).
- Follow-ups: none — no uncovered `[proof]`-tier rule identified. If a
  future boundary row is added to the proof map with concurrency/race
  content, apply the same "does it name `fast.sh`/`contract.sh`
  explicitly, and if so is the named test actually in
  `common.sh`" check used here.
- Notes: unlike Q-004's panic-path inventory, this audit found the
  proof-map/gate-script pairing already accurate — a clean grooming result
  is itself the useful output (confidence that the map isn't overclaiming
  gate coverage), matching the "documented rejection is a valid
  completion" pattern already established for Q-010-style tasks.

## 2026-07-18 09:10 Q-006 DONE [codex]
- What: built `restream`+`test_harness` via
  `scripts/build/bench-harness.sh`, confirmed the host idle
  (`pgrep -x restream; pgrep -x mediamtx; pgrep -x ffmpeg` all empty), then
  ran `scripts/build/resource-limit.sh target/bench/test_harness
  resource-sweep` serially. The harness produced 42 scenario/label
  aggregates spanning empty-baseline, single-ingest, ingest-growth (1/3/5
  pipelines, same and mixed codec), and egress-growth families (source-only,
  source+SRT, transcode, transcode+SRT, dual-transcode, dual-transcode+SRT,
  HEVC bridge) at 1/5/10 outputs per group. Recorded all 42 rows plus a
  5-row representative summary into
  `docs/agent-guidance/quality/baselines.md` under a new "Resource-sweep
  baseline — 2026-07-18" subsection (inserted before the existing
  "Historical reference — 2026-06-27" section), and seeded the top-level
  "Resource ledger" table's placeholder row with 5 of those cases. This
  harness mode does not emit a "blocked writes" metric, so that column is
  marked "not measured by this harness mode" rather than left blank or
  invented.
- Gates: `node scripts/check/docs.mjs` (Markdown touched) — pass.
- Commit: this commit (baselines.md + journal + backlog doc update only).
- Follow-ups: none. A future perf-sweep item could add a dedicated
  blocked-write counter to `resource-sweep` mode if that signal becomes
  needed; not filed speculatively since no current backlog item calls for
  it.
- Notes: RSS scales roughly linearly with output count within each scenario
  family; the two dual-transcode scenarios are the clear top of the range
  (up to ~766 MB RSS at 60 outputs), consistent with running two
  independent transcoder stages per group rather than indicating a leak.
  No ring overflows or unexpected AVIO stalls observed. This baseline
  supersedes nothing from the 2026-06-27 pass (different measurement axis:
  scale-by-output-count here vs. the fixed 15-case sizing-cut comparison
  there) — both sections are kept per `baselines.md`'s "never overwrite
  historical sections, add new dated rows" rule.

## 2026-07-18 09:40 Q-008 DONE [codex]
- What: completed `docs/layering-roadmap.md` Refactor Order item 2 ("Keep
  runtime views out of the engine core"). `StageMetrics::snapshot()`
  (`src/media/stage_metrics.rs`) and `PipeMetrics::snapshot()`
  (`src/media/pipe_metrics.rs`) now return typed `StageMetricsSnapshot` /
  `PipeMetricsSnapshot` structs (`#[derive(Serialize)]`,
  `#[serde(rename_all = "camelCase")]` to preserve the existing wire field
  names) instead of hand-building `serde_json::Value`. Updated every
  consumer: call sites inside a `json!({...})` macro needed no change
  (serde auto-converts the typed struct); call sites passing the snapshot
  as a bare `Option<serde_json::Value>`/`serde_json::Value` argument
  (`src/api_runtime_views/graph.rs`, `src/api_runtime_views/telemetry.rs`)
  were converted at the edge via `serde_json::to_value(...).unwrap_or_default()`.
  `src/api_view_models.rs`'s edge-assembly functions
  (`processing_graph_stage_node`, the `*_telemetry_row_json`/
  `single_stage_telemetry_json` family) already took typed
  `Option<serde_json::Value>` parameters by design and needed no change.
  Updated `src/media/engine_tests.rs`'s two snapshot tests
  (`pipe_metrics_snapshot_correctness`,
  the `StageMetrics` snapshot test) from JSON-index assertions to typed
  struct-field assertions. Confirmed via
  `grep -rln serde_json::Value src/media/` (excluding tests) and
  `grep -rn serde_json::json! src/media/*.rs` (excluding tests) that no
  `serde_json::Value`/`json!` usage remains anywhere in the engine core —
  the roadmap item's success condition ("engine code no longer needs to
  know UI/HTTP serialization details", "JSON assembly happens at the edge")
  is now fully met, not partially. Marked the roadmap item done in place
  and updated its "Immediate Next Steps" cross-reference (item 2) to note
  the conversion is complete rather than still "continue converting".
- Gates: `scripts/build/resource-limit.sh cargo build --profile bench
  --tests --bins` (retried once after a SIGKILL/OOM on the first attempt —
  no live pipeline process was found running; the retry succeeded cleanly,
  consistent with AGENTS.md's documented WSL2 build-memory-pressure risk
  rather than a real regression); full `scripts/build/resource-limit.sh
  cargo test --profile bench` (justified over a scoped module filter
  because the change crosses `src/media/`, `src/api_runtime_views/`, and
  `src/api_view_models.rs` module boundaries per the Inner Loop table) —
  all 13 test binaries pass, 0 failed, no warnings/panics in the log;
  `cargo fmt --all --check` — clean; `./scripts/check/api-contract.sh`
  (contract surface moved, JSON assembly relocated to the edge) — 109
  contract tests plus the `api-smoke` end-to-end check pass, wire JSON
  shape unchanged (camelCase field names preserved by
  `#[serde(rename_all = "camelCase")]`); `node scripts/check/docs.mjs`
  (Markdown touched) — pass.
- Commit: this commit (7 Rust files + `layering-roadmap.md` + journal +
  backlog doc update).
- Follow-ups: none filed. Other engine-adjacent JSON emission (e.g.
  protocol handlers, planner/media-backend parsing) is out of scope for
  this roadmap step and already tracked separately in the roadmap's
  "Current Shape" section.
- Notes: treated `PipeMetrics::snapshot()` as in-scope alongside
  `StageMetrics::snapshot()` even though the backlog item text only names
  the latter explicitly — the roadmap's stated success condition is
  binary (engine core knows about serialization, or it doesn't), so
  converting only one of the two metrics-snapshot types in the engine core
  would have left the goal formally unmet. This is the same mechanical
  pattern applied uniformly, not new indirection, so it stays within the
  layering-audit guidance to stop only when the next split would add more
  indirection than ownership clarity.

## 2026-07-18 10:20 Q-003 DONE [codex]
- What: seeded the `docs/agent-guidance/quality/baselines.md` Criterion
  benchmark ledger for `ring_buffer`, `avio_throughput`, and
  `high_performance_data_path` — the blocker (AVIO `MemoryQueue::write`
  lost-wakeup race) was already fixed and journaled earlier this run.
  Confirmed host idle (`pgrep -x restream/mediamtx/ffmpeg` all empty), then
  ran three clean serial `scripts/build/resource-limit.sh cargo bench
  --profile bench --bench <name>` invocations per suite (9 runs total).
  Each suite runs dozens of Criterion groups; since the ledger table is one
  row per suite, picked one representative low-variance headline group per
  suite rather than every group: `ring_buffer/consumer/pull_burst/8`,
  `memory_queue/write_batch/with_len`, and
  `data_path/mpegts_demux_drain/reuse_then_consume`. Recorded median (median
  of the three per-run medians) and noise (spread across the three runs)
  for each, with commit `52428c2b` and today's date.
- Gates: the ledger's own gate (three clean serial bench runs per suite on
  an idle host) — no errors/panics across all 9 runs
  (`grep -niE "error\[|panicked|error:"` on the combined log: 0 matches);
  `node scripts/check/docs.mjs` (Markdown touched) — pass.
- Commit: this commit (`baselines.md` + `backlog.md` + journal only).
- Follow-ups: none filed. The other 10 bench suites listed in the ledger
  table (`matrix_throughput`, `srt_ingest_latency`,
  `transcoder_throughput`, `hls_cost`, `hls_fmp4_cost`, `stage_feeder`,
  `stage_metrics`, `codec_conversions`, `simd_alternatives`,
  `alert_tracker`) remain unseeded — Q-003's goal named only the three
  suites now filled in, so seeding the rest is left for a future backlog
  item rather than expanded silently here.
- Notes: `high_performance_data_path`'s headline group showed a monotonic
  warm-to-fast drift across the three runs (8.39 → 9.26 → 9.66 GiB/s, ~15%
  top-to-bottom) rather than random jitter — most likely CPU
  frequency/cache ramp-up across repeated process invocations on this WSL2
  host. Recorded honestly as `±7% (see note)` in the table plus an inline
  note explaining the drift, rather than silently averaging it away or
  discarding the outlier runs, since this exceeds the ledger's own ±5%
  default regression threshold and a future perf-sweep comparison needs to
  know to expect that much spread on this specific suite. `ring_buffer`'s
  contended `ring_buffer_push_500_readers` bench was considered as the
  headline metric first but rejected for the ledger row: it swung
  30.6–36.4 µs (~19%) across the three runs, making it a poor low-noise
  anchor; `pull_burst/8` was substituted as a stable, throughput-labeled
  alternative from the same suite.

## 2026-07-18 15:38 AVIO-LOOM DONE [codex]
- What: added a loom model to `tests/avio_loom.rs` proving the
  arm-before-check `Notify` ordering that `f679a249`'s lost-wakeup fix
  (`MemoryQueue::write`/`write_cancellable`/`write_batch` in
  `src/media/avio.rs`) depends on. Loom has no built-in equivalent of
  `tokio::sync::Notify`, so the model rebuilds only the piece of its
  semantics that matters: `notify_waiters()` wakes only threads already
  registered ("armed") when it runs. `thread::park`/`unpark` alone models
  `notify_one`'s permit-persists-regardless-of-order semantics, not this —
  an explicit `waiters: Mutex<Vec<thread::Thread>>` registration list
  reintroduces the stricter ordering constraint. `ArmOrderQueue::write_fixed`
  mirrors the post-fix code shape (arm, then check-under-lock, then park on
  miss); `loom_fixed_write_survives_notify_race` asserts a single writer
  contending with one `read_one()` always completes across every
  interleaving loom explores.
- Scope note: a matching negative-control test (`write_buggy`, arming after
  the lock release, `#[should_panic(expected = "deadlock")]`) was written
  and run first. It did drive loom into its own genuine deadlock detection,
  but the panic during model unwind triggered a second panic inside loom's
  internal `Arc`/thread cleanup ("panic in a destructor during cleanup" /
  "thread caused non-unwinding panic. aborting."), which escalates to a
  hard process `abort()` — uncatchable by `#[should_panic]` and fatal to
  the whole `avio_loom` binary (it would have taken the other 4 passing
  loom tests in the file down with it). Removed the buggy shape and its
  test rather than fight loom's internal cleanup path; the historical bug
  and its non-loom regression proof already live in
  `write_wakeup_survives_lock_release_race` (`src/media/avio.rs`). Kept
  only the positive invariant, documented via a comment on the module why
  the negative control was dropped.
- Gates: `scripts/harness/loom-target.sh avio_loom` — 5/5 tests pass, 0
  failed, no abort (verified by reading the captured log directly, not just
  the wrapper's exit code). `cargo fmt --all --check -- tests/avio_loom.rs`
  — clean. Full `scripts/check/concurrency/contract.sh` (broadened since
  `avio_loom` is one of the officially tracked loom targets in
  `scripts/check/concurrency/common.sh`) — exit 0, every per-step log under
  `.local/artifacts/concurrency-contract-logs/` quiet (steps only print on
  failure; confirmed `loom-avio_loom.log` shows all 5 tests `ok`).
- Commit: this commit (`tests/avio_loom.rs` + journal only).
- Follow-ups: none filed. This closes task #9 from the standing sweep; the
  Q-NNN backlog itself is exhausted at sonnet tier (only Q-009/Q-010/Q-012
  remain, all `[opus]`).
- Notes: the abort-during-cleanup behavior is a loom 0.7.2 rough edge, not
  a bug in the model — worth remembering if a future session is tempted to
  add a should_panic-style deadlock negative control to any loom test in
  this repo: reproduce this failure mode first before assuming it'll behave
  like a normal catchable panic.

## 2026-07-18 16:20 HUNT RTMP-EGRESS-PERCENT-DECODE DONE [codex]
- What: open-ended adversarial sweep pass (not a numbered Q-item — the
  sonnet-tier Q-backlog is exhausted; Q-009/Q-010/Q-012 are `[opus]` and
  delegated to a separate agent/worktree) targeting
  `src/media/rtmp/egress_transport.rs`, one of the coverage-map's flagged
  weak files (`rtmp/egress_transport.rs` 29.84% line coverage, disposition
  "follow after RTMP session proof" — Q-016 closed that dependency this
  session). Found and fixed a real correctness bug in `parse_rtmp_url`:
  `Url::path_segments()` returns segments percent-encoded as parsed, and the
  function forwarded them straight through as the RTMP `app`/`stream_key`
  fields sent to the destination server over the wire
  (`session.request_connection(parts.app.clone())` /
  `session.request_publishing(parts.stream_key.clone())` in
  `src/media/rtmp.rs`). Any push target whose stream key legitimately
  contains a URL-reserved character (e.g. `/`, encoded as `%2F` so the URL
  parser doesn't treat it as a path separator) would reach the far end still
  escaped — corrupting the key and breaking the push. Verified the actual
  parsing behavior first via a throwaway probe test (`Url::parse` on a dozen
  adversarial inputs — leading/trailing whitespace, mixed-case scheme, empty
  authority, out-of-range port, unterminated IPv6 literal, embedded
  userinfo, percent-encoded segments, trailing slash) before writing any
  fix, so the new tests assert real crate behavior rather than assumptions.
  Fix: percent-decode `app` and the joined `stream_key` with
  `percent_encoding::percent_decode_str(..).decode_utf8_lossy()` (lossy, not
  fallible — invalid percent-sequences or non-UTF8 bytes must degrade to
  replacement characters, not panic or reject an otherwise-valid URL).
  `percent-encoding` was already in `Cargo.lock` as a transitive dependency
  of `url`; promoted it to a direct dependency (`Cargo.toml`) since the code
  now calls it directly. Also promoted `format_host_port` (same file) from
  private to `pub(super)` — it had zero test coverage and its bracket-vs-no-
  bracket branching for IPv6 literals is exactly the kind of logic this
  sweep exists to catch; testing it needed visibility beyond the file.
- Gates: `scripts/build/resource-limit.sh cargo test --lib rtmp` — 109/109
  pass (12 new `parse_rtmp_url_*` cases: percent-decoded slash/space/plus,
  invalid percent-sequence doesn't panic, trailing-slash behavior
  documented, query/fragment stripped, userinfo dropped without leaking,
  empty authority / out-of-range port / unterminated IPv6 all rejected
  cleanly, case-insensitive scheme, whitespace trimmed; 4 new
  `format_host_port_*` cases: plain hostname, IPv4 literal, bare IPv6 gets
  bracketed, already-bracketed IPv6 not double-wrapped). `cargo fmt --all
  --check` — clean. `scripts/build/resource-limit.sh cargo clippy --lib --
  -D warnings` — clean. Broadened to full
  `scripts/build/resource-limit.sh cargo test` since this changes
  `Cargo.toml`/`Cargo.lock` (a shared-contract surface, not just one
  module) — 1106 lib tests + all integration/doctest binaries, 0 failed.
- Commit: this commit (`Cargo.toml`, `Cargo.lock`,
  `src/media/rtmp/egress_transport.rs`, `src/media/rtmp/tests.rs`, journal).
- Follow-ups: none filed. `rtmp/egress_transport.rs`'s remaining untested
  surface (`connect_rtmp_egress_stream`, `connect_tcp_with_options`,
  `rtmp_sender_quality`'s TCP-stats branch) needs a live socket/TLS
  handshake or real `/proc` TCP info to exercise meaningfully — not a good
  fit for a crafted-bytes unit test; would need the live harness
  (`test_harness` correctness modes) or an explicit design note if picked
  up later.
- Notes: this is "the hunting" workstream per the user's explicit
  instruction to continue open-ended adversarial sweep work beyond the
  closed Q-backlog while a separate opus-tier agent handles
  Q-009/Q-010/Q-012 in `.local/worktrees/perf-sweep-opus-20260718`. Next
  candidates from the same coverage-map lead list (still open, not yet
  investigated): `srt_monitor.rs` (43.75%), `engine_snapshots.rs` (68.59%,
  "snapshot error branches"), `mpegts_probe.rs` (72.92%, "probe/reporting
  paths").

## 2026-07-18 16:40 Q-009 DONE [opus]

- What: Eliminated the per-packet copy in the MPEG-TS mux → SRT egress
  accumulator path (backlog Q-009). The 2026-06-27 CPU profile named a
  two-copy shape (FFmpeg AVIO output buffer → `ts_accum`, `memmove` 3.28% +
  `VecDeque::extend` 0.43%); that AVIO path has since been replaced by the
  pure-Rust `TsMuxer`, but the equivalent copy survived as the muxer's
  internal `output: Vec<u8>` scratch being `extend_from_slice`'d into the SRT
  egress burst accumulator once per packet
  (`srt_egress.rs::start_shared_ts_muxer`). Added
  `TsMuxer::mux_packet_into` and `mux_packet_by_stream_idx_into`, which append
  TS packets directly into a caller-owned `&mut Vec<u8>`; the write path
  (`mux_packet_at`, `write_pat/pmt/sdt`) now takes that accumulator instead of
  writing to `self.output`. The standalone `mux_packet` /
  `mux_packet_by_stream_idx` APIs are preserved for the ~30 single-packet
  callers (tests, feeder, hls_cost, matrix_throughput, simd_alternatives) via
  an O(1) `mem::take` of the internal scratch, so no correctness contract
  moved. The egress feeder now sizes the accumulator as `Vec<u8>` and freezes
  it with `Bytes::from(vec)` (O(1) ownership transfer) instead of
  `BytesMut::freeze()`; per-chunk `.slice()` publication is unchanged.
- Gates: `cargo test --lib mpegts` (74 passed) and `cargo test --lib srt`
  (91 passed) green — protocol correctness (PAT/PMT/SDT insertion, PES
  packetization, continuity, DTS enforcement, mux↔demux round-trip proptest)
  unchanged. `cargo fmt --all --check` clean. Before/after microbenchmark
  (`high_performance_data_path`, WSL2 idle host, 100 samples): the existing
  `batch_accumulate_write` variant models the old `mux_packet` +
  `extend_from_slice` shape (10.767 µs / 2.972 Melem/s); the new
  `batch_mux_into_write` variant models `mux_packet_into` (10.062 µs /
  3.180 Melem/s) — −6.5% latency / +7.0% throughput, non-overlapping 95% CIs.
  Core `data_path/mpegts_mux/mux_all_packets` unchanged within noise
  (478 µs, 12.6 GiB/s). Numbers + ledger row in `baselines.md` (§ Benchmark
  ledger, § Standing optimization targets → Q-009 result).
- Commit: this commit (`src/media/mpegts.rs`, `src/media/srt_egress.rs`,
  `benches/high_performance_data_path.rs`, `baselines.md`, `backlog.md`,
  journal).
- Follow-ups: none filed. The recording/HLS feeders still use the
  single-packet `mux_packet_by_stream_idx` (they accumulate into their own
  segment/manifest buffers with a different lifetime); converting them to the
  `_into` API is a possible future micro-win but was out of scope for the SRT
  egress hot path this item names.
- Notes: contabo VPS (the designated hardware-PMU box) was unavailable — a
  ~2-day-old external MSR-style workload (3× restream / 3× mediamtx / 7×
  ffmpeg, 175k–202k s elapsed) was live, so the kill-check was non-empty and
  those processes were not this session's to kill. Measurement therefore ran
  on the idle WSL2 host with wall-clock Criterion timing, which is sufficient
  for this copy-elimination micro-decision (no PMU counters needed); the MSR
  receiver-scale proof named in the item's aspirational gate list is deferred
  until the VPS is idle and is not required to accept a strictly-fewer-copies
  change with green protocol tests and a non-overlapping-CI microbench win.

## 2026-07-18 16:45 HUNT SRT-MONITOR-OVERFLOW DONE [codex]
- What: continued "the hunting" against the next coverage-map candidate,
  `src/media/srt_monitor.rs` (43.75% line coverage). Found and fixed a real
  panic path in `monitor_listener_socket`: `crit_threshold = (configured_buf
  * 3) / 4` is computed synchronously, before the function's first
  `.await`, with no overflow protection. An `effective_udp_recv_capacity`
  near `u64::MAX` overflows the multiplication and panics
  (`attempt to multiply with overflow`) the instant the future is first
  polled — i.e. immediately on task spawn, not after any delay. Verified
  with a throwaway probe test calling `monitor_listener_socket(0, stats,
  u64::MAX)` under a 50ms timeout before writing the fix, confirming the
  panic fires at `src/media/srt_monitor.rs:62` exactly as read from the
  source, not a guessed line. Today's one caller (`srt.rs`) sources this
  value from an `i32` kernel sockopt cast to `u64` (bounded well under the
  overflow threshold), so this isn't reachable in the current call graph —
  but `monitor_listener_socket` is `pub(super)`, not defensively scoped to
  that one caller, and AGENTS.md's media-rules invariant is unconditional
  ("No internal or external failure path may crash the engine"), so this
  gets hardened regardless of current reachability. Fix: replace the plain
  multiply with `configured_buf.saturating_mul(3) / 4` — an extreme input
  saturates to a very high (effectively unreachable) threshold instead of
  overflowing.
- Gates: `scripts/build/resource-limit.sh cargo test --lib srt` — 92/92
  pass, including the new
  `monitor_listener_socket_extreme_capacity_does_not_panic` regression test
  (asserts the call survives past a 50ms timeout instead of panicking
  before the first `.await`). `cargo fmt --all --check` — clean (after
  running `cargo fmt --all` to fix the new test's formatting and a stray
  blank-line diff left over from the prior probe-test removal).
  `scripts/build/resource-limit.sh cargo clippy --lib -- -D warnings` —
  clean. Broadened to full `scripts/build/resource-limit.sh cargo test`
  since arithmetic-safety invariants in a shared media-engine helper are a
  cross-module concern, not scoped to one file — 0 failed (all "FAILED" /
  "panicked" greps on the full run output were pre-existing, unrelated
  fixture/log lines like a deliberately-injected 502 in an upload test).
  `node scripts/check/docs.mjs` — 68 Markdown files pass.
- Commit: this commit (`src/media/srt_monitor.rs`, `src/media/srt_tests.rs`,
  journal).
- Follow-ups: none filed. `srt_monitor.rs`'s `read_udp_socket_stats` reads a
  hardcoded `/proc/net/udp` path rather than an injectable source, so its
  parsing logic can only be probed indirectly against the real host's proc
  file (already covered by the pre-existing
  `reads_udp_socket_stats_for_listener_port` test) — refactoring it to take
  an injectable reader purely to unit-test malformed `/proc` content would
  be scope creep beyond what this bug hunt needs.
- Notes: continuing "the hunting" per the user's "you go ahead with the
  hunting" instruction. Next candidates from the same coverage-map lead
  list (still open, not yet investigated): `engine_snapshots.rs` (68.59%,
  "snapshot error branches"), `mpegts_probe.rs` (72.92%, "probe/reporting
  paths").

## 2026-07-18 17:20 HUNT ENGINE-SNAPSHOTS-POISON DONE [codex]
- What: continued "the hunting" against the next coverage-map candidate,
  `src/media/engine_snapshots.rs` (68.59% line coverage, annotated "snapshot
  error branches"). Read the file in full: it's mostly defensive plumbing
  (`Option`/`HashMap` lookups) with no obvious bug surface, so rather than
  write coverage-padding tests I looked for a genuine untested behavior.
  Found one: `active_egress_diag_snapshots` reads `egress.phase`,
  `.target_addr`, and `.last_error` — all `std::sync::Mutex` — via the
  codebase's standard poison-recovery idiom
  `.lock().unwrap_or_else(|e| e.into_inner())`, and
  `active_ingest_diag_snapshot` does the same for `.keyframe_times`. None of
  it had a dedicated regression test proving the recovery path actually
  works for these fields. Two existing tests
  (`stale_egress_error_cannot_poison_replacement_attempt`,
  `stale_ingest_disconnect_cannot_poison_replacement_attempt`) use "poison"
  in their names but — confirmed by reading their full bodies — test a
  different concept (a superseded attempt ID clobbering a replacement's
  state), not real mutex lock poisoning, so this was a genuine gap, not
  already-covered ground.
  Added `active_egress_diag_snapshots_recovers_from_poisoned_locks` in
  `src/media/engine_tests.rs`, following the poison-test idiom established
  in `src/media/avio.rs`'s and `src/media/ring_buffer_tests.rs`'s own test
  modules (`EXPECTED_PANIC_HOOK_LOCK` + `ScopedSilentPanicHook` to suppress
  the intentional panic's default backtrace noise). That idiom is defined
  per-test-module in this codebase rather than shared, so a third copy was
  added locally in `engine_tests.rs` rather than exporting the existing ones
  out of `avio.rs`. The test registers a real egress attempt, clones its
  `phase`/`target_addr`/`last_error` `Arc<Mutex<_>>` handles, poisons each
  from a separate panicking `std::thread::spawn` (one of them also mutates
  the guarded value before panicking, so the recovery path is observably
  carrying the poisoned-in value forward, not silently resetting it), then
  calls `active_egress_diag_snapshots` and asserts it returns the egress
  with the correct recovered `target_addr` instead of panicking or dropping
  the entry.
  Outcome: this did not find a new bug — the poison-recovery idiom was
  already correct everywhere it's used here, consistent with it being a
  previously-fixed pattern (per `ring_buffer_tests.rs`'s
  `reader_drop_cleans_up_on_poisoned_mutex` comment referencing an earlier
  fix in `ring_buffer.rs`). This closes a real coverage/proof gap on
  AGENTS.md's unconditional invariant ("No internal or external failure
  path may crash the engine; isolate faults and surface errors") rather
  than fixing a fresh defect, and is recorded as such rather than inflated
  into a bug find.
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  active_egress_diag_snapshots_recovers` — 1/1 pass.
  `scripts/build/resource-limit.sh cargo test --lib media::engine::tests::`
  — 121/121 pass. `cargo fmt --all --check` — clean.
  `scripts/build/resource-limit.sh cargo clippy --lib -- -D warnings` —
  clean. Broadened to full `scripts/build/resource-limit.sh cargo test`
  since the change touches a cross-cutting mutex-poison-recovery contract
  used by multiple `engine.rs` snapshot paths, not one isolated file — 1108
  lib tests + 135 + 109 + 21 + 14 + 18 + 23 + 4 + 2 + 10 + 14 integration
  tests all passed, 0 failed.
- Commit: this commit (`src/media/engine_tests.rs`, journal).
- Follow-ups: none filed. `mpegts_probe.rs` (72.92%, "probe/reporting
  paths") remains the next open candidate on the coverage-map lead list and
  has not yet been investigated.
- Notes: continuing "the hunting" per the user's "you go ahead with the
  hunting" instruction.

## 2026-07-18 17:20 Q-010 DONE [opus]

- What: Evaluated slab/pool allocation for the per-packet
  `Arc<MediaPacket>` (backlog Q-010, `_int_malloc` 0.87% self-time in the
  2026-06-27 profile). Decision: REJECTED — no runtime code changed; the
  `Arc::new(MediaPacket)` stays. Surveyed the allocation sites first:
  `RingBuffer::push`/`push_batch` do `Arc::new(packet)` then a slot
  `ArcSwapOption::store` (`ring_buffer.rs:489,519`), and readers `load_full()`
  take an `Arc<MediaPacket>` with an unbounded lifetime. `MediaPacket` is 56 B
  (`repr(C)`); with the 16-B Arc control block the request is 72 B, served from
  glibc's per-thread `tcache` fast path.
- Gates: `ring_buffer/producer` Criterion bench, WSL2 idle host, kill-check
  clean (`pgrep -x restream/mediamtx/ffmpeg` empty), 100 samples. Whole-`push`
  time (includes `Arc::new` + `ArcSwap` store): `push_one_at_a_time` 142.05 ns/1,
  541.34 ns/4 (135 ns/elem), 1.086 µs/8 (136 ns/elem); `push_batch` 145.71 ns/1,
  519.09 ns/4 (130 ns/elem). Per-element cost is flat 1→8 and `push` ≈
  `push_batch` within noise, so batching already amortizes the path — no
  per-burst allocation win remains for a pool to capture. No code changed, so no
  loom/unit ownership proofs were needed.
- Commit: this commit (`baselines.md` § Q-010 result + standing-target row,
  `backlog.md` Q-010 status, journal). No `src/` changes.
- Follow-ups: none. If a future hardware-PMU MSR profile shows `_int_malloc`
  climbing materially above the current 0.87%, revisit — but only with a design
  that keeps `Arc` reclamation semantics.
- Notes: three reasons the pool loses. (1) Magnitude — 0.87% self-time on a
  tcache-served, lock-free O(1) size class; a pool's best case only replaces an
  already-fast path. (2) Ownership — reclamation is intrinsically
  last-reader-drop (readers hold the `Arc` across await points and threads for
  arbitrary time); `Arc` + the global allocator already do exactly that. A
  custom slab must replicate the last-drop hook *plus* a synchronized
  cross-thread freelist, because producers (SRT/RTMP ingest threads) and
  consumers (egress/HLS/recording tasks) run on different threads — every
  reclaim becomes a cross-thread free contending on the pool lock, versus tcache
  handling the common same-thread free lock-free. It trades a lock-free path for
  a contended one on a cold path. (3) Safety — a slot-reusing pool risks
  use-after-free / ABA if a reader outlives the intended lifetime, violating the
  engine no-crash invariant; `Arc` makes that impossible by construction. Full
  write-up in `baselines.md` § Q-010 result. Contabo VPS still carried the
  ~2-day external MSR workload, but this micro-decision needs no PMU counters —
  the wall-clock bench plus the ownership/safety analysis is sufficient to
  reject.

## 2026-07-18 17:55 Q-012 DONE [opus]

- What: Final decision on in-process CPU affinity for Tokio/SRT/RTMP thread
  families (backlog Q-012, which the 2026-07-12 series had narrowed to "an
  opt-in runtime affinity design"). Decision: REJECTED as a runtime feature —
  CPU partitioning stays a process/cgroup concern (systemd `CPUAffinity`,
  Docker `--cpuset-cpus`, Kubernetes CPU manager). No runtime affinity code
  exists or is added. Tightened `docs/configuration.md` § Linux Service
  Placement from "the runtime does not currently pin thread families / keep
  experiments outside production" to a definite statement that the
  process/cgroup layer is the supported mechanism and in-process pinning was
  evaluated and rejected, with a pointer to the evidence. Added
  `baselines.md` § Q-012 decision consolidating the recorded numbers and the
  rationale.
- Gates: none run this session — the decision changes no code. It rests on the
  already-recorded MSR evidence (2026-07-12 VPS series): external `taskset`
  partition (SRT→CPU 0-1, other→2-5) was a real win (2.051 cores, IPC 0.420,
  16.25% cache misses, 4.330 K/s ctx, 288.5/s migrations) versus default
  runtime (2.321 cores, 20.80% cache, 7.663 K/s ctx, 920.3/s migrations), but
  the in-process scanner did NOT reproduce it (2.45/2.42 cores, ~20.6-20.9%
  cache, ~7.7-8.0 K/s ctx) despite a thread census proving the masks were
  applied. `node scripts/check/docs.mjs` clean.
- Commit: this commit (`docs/configuration.md`, `baselines.md` § Q-012
  decision, `backlog.md` Q-012 status, journal). No `src/` changes.
- Follow-ups: none. If a future host demonstrates the partition win robustly
  under a supported orchestration cpuset, that is a deployment recipe, not a
  runtime feature. A true 12h soak remains separate from these non-soak MSR
  ramps.
- Notes: three reasons the process/cgroup layer is correct and in-process
  pinning is not. (1) Robustness — the scanner is a one-shot `/proc/self/task`
  pass, but Tokio continuously spawns replacement/blocking threads (census
  showed a `restream-tokio` family in the 60s with only two hot scheduler
  worker identities); new threads inherit their creator's mask at clone time,
  so a one-shot partition erodes as the population turns over, while a cpuset is
  kernel-enforced on every present and future thread for the whole process
  lifetime. (2) Layering — a cpuset derives from the effective CPU mask/cgroup
  quota automatically and is container-aware; in-process host-CPU masks are not
  and would fight orchestration. (3) Cost/benefit — the win needs a clean
  default run and a whole-window hold, exactly what a launch-time cpuset gives
  for free and what fragile in-process re-pinning cannot guarantee; adding
  thread-lifecycle placement code plus its concurrency-proof burden to chase an
  effect the supported layer already captures is negative-value. No new
  measurement was run: WSL2 has no PMU and the Contabo VPS carried a ~2-day
  external MSR workload (kill-check non-empty, not this session's to kill);
  re-running the scanner would only re-confirm the recorded negative. This
  aligns with and finalizes the 2026-07-12 codex follow-ups (RUNTIME AFFINITY
  PROTOTYPE REJECTED, TOKIO BLOCKING CAP / KEEPALIVE PROTOTYPES REJECTED, MSR
  FULL FINAL PASS), which had already recommended systemd placement and left
  Q-012 "narrowed"; this entry closes it.
## 2026-07-18 18:05 HUNT MPEGTS-PROBE-AUDIO-BOUNDARY DONE [codex]
- What: continued "the hunting" against the last open coverage-map
  candidate, `src/media/mpegts_probe.rs` (72.92% line coverage, annotated
  "probe/reporting paths"). Read the file in full (812 lines): the H.264/H.265
  SPS parsing paths are already heavily hardened from Q-019/task#10 — checked
  arithmetic throughout, plus existing proptests in `mpegts_tests.rs`
  (`probe_video_never_panics`,
  `probe_video_h264_truncation_never_yields_partial_metadata`,
  `probe_video_h265_truncation_never_yields_partial_metadata`) already prove
  fail-closed behavior for random and truncated bitstreams. `h264_is_keyframe`
  / `h265_is_keyframe` also already have dedicated empty-payload and
  no-start-code edge-case tests.
  `probe_audio` (ADTS header parsing, the other public probe function in the
  file) was the exception: only one happy-path test (`adts_probe`) existed.
  Reading the function found four branches with no dedicated test: the
  `pes_payload.len() >= 7` length guard, the sync-word check
  (`pes_payload[0] == 0xFF && (pes_payload[1] & 0xF0) == 0xF0`), the
  `sample_rate_idx < SAMPLE_RATES.len()` bounds check (reserved indices
  13/14/15 are representable in the 4-bit field but not in the 13-entry
  table), and the `channels == 7 → 8` ADTS special-case remap. None of these
  can currently panic (all indexing is guarded), but none had a regression
  test proving the guard actually rejects the malformed/boundary input rather
  than, say, silently parsing garbage or leaving a stale value from a
  previous call.
  Added `adts_probe_boundary_and_malformed_inputs` in `mpegts_tests.rs`,
  covering: empty payload, a payload exactly one byte short of the 7-byte
  minimum, a payload that fails the sync-word check despite being long
  enough, a reserved `sample_rate_idx` of 13 (asserts `sample_rate` stays 0
  and `audio_meta_complete` reports incomplete despite a valid `profile`),
  and a `channel_config` of 7 (asserts it maps to 8 channels and
  `audio_meta_complete` reports complete).
  Outcome: no new bug — every guard already behaves correctly — but this
  closes the last real gap on this file's public parsing surface: previously
  only the happy path was pinned, so a regression in any of these four
  guards (e.g. an off-by-one on the length check, or dropping the
  channels==7 remap) would have shipped silently. Recorded honestly as a
  coverage/proof-gap closure, not a bug fix, consistent with the discipline
  used for `engine_snapshots.rs`.
- Gates: `scripts/build/resource-limit.sh cargo test --lib mpegts` — 75/75
  pass (all mpegts module tests, including the new one). `cargo fmt --all
  --check` — clean after one formatting fixup. `scripts/build/resource-limit.sh
  cargo clippy --lib -- -D warnings` — clean. `node scripts/check/docs.mjs` —
  clean. Ran full `scripts/build/resource-limit.sh cargo test --lib`: 1107
  passed, 2 failed
  (`external_transcoder::tests::chained_hevc_preview_stages_emit_live_h264_packets`,
  `external_transcoder::tests::hevc_scaled_rtmp_audio_routes_emit_both_selected_tracks`).
  Confirmed unrelated to this change — `git status` shows only
  `src/media/mpegts_tests.rs` modified, and both failing tests spawn real
  ffmpeg subprocesses under `--test-threads` parallelism; re-ran just
  `external_transcoder::tests` with `--test-threads=1` and both passed
  (20/20), confirming pre-existing resource-contention flakiness under full
  parallel load, not a regression from this change. Did not broaden to a
  concurrency proof gate — this change is a single-file boundary-value test
  addition with no concurrency, lifecycle, or thread-hop surface.
- Commit: this commit (`src/media/mpegts_tests.rs`, journal).
- Follow-ups: none filed. Both previously-identified coverage-map lead-list
  candidates (`engine_snapshots.rs`, `mpegts_probe.rs`) are now exhausted;
  the next hunting step is to either re-derive a new coverage-map lead list
  or continue open-ended hunting without one.
- Notes: continuing "the hunting" per the user's "you go ahead with the
  hunting" instruction. Also noted in passing: the delegated background
  agent (workstream A, Q-009/Q-010/Q-012 on
  `codex/perf-sweep-opus-20260718`) completed independently during this
  hunt — not touched, per instructions.

## 2026-07-18 20:45 HUNT FFMPEG-STAGE-PLAN-CODEC-DEFAULT DONE [codex]

- What: after PR #55 (combined perf-sweep + adversarial-hunt checkpoint)
  merged, re-derived a fresh low-coverage lead list via `cargo llvm-cov
  --summary-only --lib` since both prior coverage-map leads
  (`engine_snapshots.rs`, `mpegts_probe.rs`) were exhausted and the formal
  Q-001–Q-022 backlog is fully done. Evaluated two candidates:
  `src/media/hls/preview_graph.rs` (~26% line coverage, async, requires a
  full `MediaEngine` + `StageRuntimeManager` harness to test meaningfully)
  and `src/media/ffmpeg/stage_plan.rs` (166 lines, ~25% line coverage, pure
  synchronous planner code, zero dedicated test file). Picked the latter as
  tractable and non-duplicative.
  While reading `stage_plan.rs`, noticed the repo has *two* distinct
  `VideoCodecKind` types with different semantics: `domain::output_spec::
  VideoCodecKind` (3 variants incl. `Unknown`, tested) and `media::ffmpeg::
  stage_plan::VideoCodecKind` (2 variants, no `Unknown`) — the latter's
  `from_codec_name` silently defaults *any* unrecognized string (empty,
  "vp9", "av1", ISOBMFF fourccs like "hvc1"/"hev1", homoglyphs, garbage) to
  `H264` rather than erroring or having a third state. Traced every call
  site (`stage_runtime.rs`, `ffmpeg_process.rs`) and confirmed the only
  codec-hint strings that actually flow through this code path in practice
  are the literals `"h264"`/`"hevc"` set at ingest — so the silent-default
  behavior is unreachable with malformed input today, not a live bug, but
  it was completely unpinned: nothing proved the exact-match/case-fold
  contract, and a future caller passing an ISOBMFF fourcc or a raw ffprobe
  codec string would silently mis-plan HEVC content as H264 passthrough
  with no test to catch it.
  Added a `#[cfg(test)] mod tests` block directly in `stage_plan.rs`
  (matching the sibling-file convention in `stage_input.rs`/
  `stage_output.rs`/`timeline.rs`, none of which use a separate test file):
  `from_codec_name_matches_hevc_spellings_case_insensitively` (all six
  case variants of "hevc"/"h265"/"h.265"), `from_codec_name_defaults_
  unrecognized_inputs_to_h264` (empty string, valid-but-non-hevc codec
  names, near-miss spellings with stray whitespace, ISOBMFF fourccs),
  `from_codec_name_handles_malformed_and_extreme_input` (Cyrillic
  homoglyph of "h", embedded NUL byte, a 64KB garbage string, and a string
  that contains "hevc" as a substring but isn't an exact match — proving no
  accidental substring matching), `as_str_round_trips_through_from_codec_
  name`, and defaults assertions for both `FfmpegStagePlan::video_preset`
  and `::hevc_to_h264` convenience constructors (startup/timeline policy
  fields, `output_codec` override behavior).
  Outcome: no bug found — recorded as a proof-gap closure on a previously
  fully-untested pure function, consistent with the `mpegts_probe.rs` and
  `engine_snapshots.rs` entries above. The domain-duplication observation
  (two `VideoCodecKind` types) is left as-is; both are used correctly
  within their own layers and collapsing them would be a layering change
  outside this hunt's scope.
- Gates: `scripts/build/resource-limit.sh cargo test --lib stage_plan` —
  7/7 new tests pass (1108 pre-existing filtered out, unaffected).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all --check` — clean.
  `./scripts/check/source-audit.sh` — clean. Single-file, non-lifecycle,
  non-concurrency pure-function test addition — did not broaden to full
  `cargo test` or a concurrency proof gate.
- Commit: `fcb5a755` on `codex/adversarial-hunt-round2-20260718` (branched
  from `origin/master` post-#55-merge).
- Follow-ups: none filed.
- Notes: hit and fixed a worktree/branch-naming mixup mid-session — a
  local branch `codex/adversarial-hunt-continued-20260718` created in a
  prior turn ended up checked out in a *different* worktree (the main repo
  checkout) than this session's actual working directory, so a first
  attempt at this commit landed on the stale, already-squash-merged
  `codex/adversarial-test-sweep-20260717` branch instead. Fixed by renaming
  this worktree's local branch to `codex/adversarial-hunt-round2-20260718`,
  resetting it to `origin/master`, and cherry-picking the one genuinely new
  commit onto the clean base before pushing — no work was lost, and the
  other worktree/branch was left untouched since it may be in use by
  another agent session.

## 2026-07-18 21:15 HUNT HLS-PREVIEW-GRAPH-CANCEL DONE [codex]
- What: investigated `src/media/hls/preview_graph.rs::resolve_hls_preview_graph`
  (zero prior test coverage) for a suspected cancellation-ordering race: the
  function checks `cancel.is_cancelled()` exactly once per loop iteration,
  *after* resolving the codec but *before* branching into the work that
  plans and spawns an HEVC→H.264 preview transcoder stage via
  `StageRuntimeManager::ensure_stage`/`spawn_preview_stage` — with no further
  cancellation check during that stage-creation work itself.
  Traced the full lifecycle to determine whether a late cancellation in that
  window can orphan a created-but-unwanted preview stage. Found:
  `ensure_stage` (`src/media/stage_runtime.rs:71`) mints its own independent
  `CancellationToken` for the stage runtime (not the caller's `cancel`
  argument — `ensure_stage` doesn't even take one), so the preview stage's
  lifecycle is never tied to the HLS segmenter's cancellation token in the
  first place. The segmenter's own shutdown path
  (`start_hls_fmp4_segmenter` in `fmp4.rs:334`, called with
  `planned_stage_key: None` from `preview.rs`) tears down a *different*
  stage key (`StageKind::hls()`), never the `StageKind::Preview` key the
  graph resolver created — so per-segmenter cancellation was never going to
  reap this stage. Instead, preview transcoder stages are pooled,
  per-pipeline shared resources reaped by two independent, coarser-grained
  mechanisms: `MediaEngine::cleanup_pipeline_stages` (eager sweep on
  pipeline removal, `engine.rs:991`) and
  `MediaEngine::sweep_unused_transcoder_stages` (periodic reconciler sweep
  against the currently-planned key set, `engine.rs:1008`). A stage created
  microseconds before a caller's cancellation fires is not leaked — it
  becomes part of the shared pool and is pruned by the next reconciler pass
  or pipeline teardown like any other stage the current plan no longer
  wants.
  Outcome: not a bug — the single-check-per-iteration pattern is consistent
  with the architecture (stages are reconciled independently of any one
  caller's lifetime, not synchronously owned by it). Closed the coverage
  gap instead: added two direct regression tests for
  `resolve_hls_preview_graph`'s two `None`-returning edge paths, which had
  no direct test before (only indirectly exercised via `preview.rs`'s
  higher-level `ensure_hls_preview_runtime` tests, which never await the
  resolver's own completion). `returns_none_immediately_when_cancelled_
  before_codec_resolves` pre-cancels the token on a pipeline with no
  resolvable codec and asserts the paused clock never advances (proving the
  cancellation check fires on the very first iteration, before the 100ms
  poll sleep). `returns_none_after_deadline_when_codec_never_resolves`
  leaves the token uncancelled on the same never-resolves pipeline and
  asserts the paused clock advances at least the full 3s deadline before
  returning `None`. Both use `#[tokio::test(start_paused = true)]` so the
  real 3s deadline costs zero wall-clock time in the suite.
- Gates: `scripts/build/resource-limit.sh cargo test --lib preview_graph` —
  2/2 new tests pass (4 pre-existing planner tests in the same filter
  unaffected). `scripts/build/resource-limit.sh cargo clippy --lib
  --benches -- -D warnings` — clean. `cargo fmt --all --check` — clean
  (after `cargo fmt --all` reformatted one over-length line). Single-file,
  non-lifecycle-signature-changing test addition (no production code
  touched) — did not broaden to full `cargo test` or
  `scripts/check/concurrency/contract.sh`.
- Commit: `25390121` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. The two independent `StageKey`s in play here
  (`StageKind::Preview` for the transcoder, `StageKind::hls()` for the
  segmenter) and the two separate stage-pool sweep mechanisms are
  intentional existing architecture, not a gap to close.
- Notes: none.

## 2026-07-18 21:45 HUNT SRT-STREAM-ID-ADVERSARIAL DONE [codex]

- What: continued the hunt to `src/media/srt_stream_id.rs` (90 lines,
  zero dedicated test module — coverage existed only indirectly, via
  black-box happy-path cases in `srt_tests.rs`). This module parses
  untrusted, client-supplied SRT handshake stream IDs
  (`parse_srt_stream_id`, `normalize_srt_stream_key`, `percent_decode`,
  `strip_query`) and is directly on the network-facing SRT connection path
  (`srt.rs` uses the parsed mode to distinguish publisher vs. reader
  connections); AGENTS.md's Media Rules explicitly names "Normalize SRT
  Stream IDs before lookup" as a core invariant.
  Read `srt_tests.rs` in full first to avoid duplicating existing coverage
  (~17 cases across 4 tests already covered common-tool stream ID formats,
  encoding normalization, and slash-preserving percent-decoding). Added a
  `#[cfg(test)] mod tests` block directly in `srt_stream_id.rs` (matching
  the sibling-file convention from the `stage_plan.rs` hunt) targeting
  genuinely uncovered adversarial edges: `percent_decode` truncated/
  malformed `%` escapes (trailing `%`, single hex digit, non-hex digits —
  must fall back to literal, not panic or desync the scan position),
  case-insensitive hex digits, single-layer-only decoding (`%2525` →
  `%25`, not `%`), lossy UTF-8 fallback on invalid byte sequences from
  `%FF` (must not panic), embedded NUL byte preservation; the two-pass
  `strip_query` interaction in `normalize_srt_stream_key` where a
  percent-encoded `?` (`%3F`) is only revealed after decoding and still
  gets stripped by the second `strip_query` call, meaning a stream key can
  never contain a literal `?` in any encoding; `parse_srt_stream_id`
  whitespace/NUL-only-input handling, and the `#!::` bracket-format
  parser's duplicate-key last-wins semantics, `=`-containing values,
  malformed (no-`=`) parts being silently skipped, case-sensitive keys and
  mode values, and an empty-rest `#!::` input.
  The highest-value, most non-obvious finding (documented via a dedicated
  test, not filed as a bug): any raw (non-`#!::`) stream ID containing a
  colon has everything before the first colon discarded as a candidate
  mode marker, *even when that prefix is not a recognized mode keyword* —
  e.g. `"abc:def"` silently loses `"abc:"` and yields stream key `"def"`
  under the default Publish mode, and `"Play:key"` (wrong case) likewise
  loses `"Play:"` yet still defaults to Publish rather than Read. This is
  existing, intentional-looking behavior (the split happens unconditionally
  before the recognized-keyword check), not a bug to fix — but it was
  completely unpinned, so a future refactor could silently change it
  without any test noticing. Pinned it explicitly instead of filing a fix,
  since changing SRT Stream ID parsing semantics is a protocol-compat
  decision, not an adversarial-hunt scope call.
  Caught one own-test-authoring bug during the red/green cycle: an initial
  test asserted `"\0 \0 \0"` (NUL-space-NUL-space-NUL) parses to an empty
  stream key, but `trim_matches('\0')` only strips NUL runs from the two
  string *ends*, not interior occurrences, so the actual result is a
  single interior NUL byte, not empty. Fixed by narrowing the empty-input
  test to inputs that genuinely collapse (pure NUL runs, pure whitespace,
  NUL-padded whitespace) and adding a separate test,
  `parse_srt_stream_id_preserves_interior_nul_bytes`, that pins the
  correct (interior-NUL-survives) behavior explicitly.
- Gates: `scripts/build/resource-limit.sh cargo test --lib srt_stream_id`
  — 25/25 pass (21 new + 4 pre-existing `srt.rs`-level black-box tests in
  the same filter unaffected). `scripts/build/resource-limit.sh cargo
  clippy --lib --benches -- -D warnings` — clean. `cargo fmt --all` /
  `--check` — clean. Single pure-function module, not listed among the
  Inner Loop table's lifecycle-sensitive files (`engine.rs`, `srt.rs`,
  `ts_chunk_ring.rs`, `avio.rs`, `recording.rs`, `file_ingest.rs`,
  `external_transcoder.rs`) and no production code was touched — did not
  broaden to `scripts/check/concurrency/contract.sh` or full `cargo test`.
- Commit: `8dd9cb9f` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. The unrecognized-colon-prefix-still-stripped
  behavior and the double-strip-query-after-decode interaction are now
  pinned by tests; no fix is warranted without a protocol-compatibility
  decision from a human.
- Notes: none.

## 2026-07-18 22:05 HUNT STAGE-METRICS-COUNTER-BOUNDARIES DONE [codex]

- What: swept the remaining low-coverage lead list
  (`stage_registry_access.rs`, `engine_hls.rs`, `snapshots.rs`,
  `stage_metrics.rs`, `pipe_metrics.rs`, `ingest_auth.rs`) for the next
  hunt target. Ruled out two before writing anything: `ingest_auth.rs` is
  a pure trait/type-definition file with zero branching logic, nothing to
  adversarially test; `stage_registry_access.rs` is thin async CRUD glue
  over engine registries whose one piece of real logic — SRT egress muxer
  shard assignment/release — delegates to `SrtMuxerShardPool` in
  `engine_registries.rs`, which `engine_stage_tests.rs` already covers
  exhaustively, including a proptest model-based shard-lifecycle test.
  Picked `src/media/stage_metrics.rs` (106 lines, zero dedicated tests):
  lock-free `AtomicU64` throughput counters updated on the packet hot path
  and read by the `/graph` operator-visibility endpoint. Despite being
  hot-path code (no benchmark needed per Hot-Path Rules — plain atomic
  `fetch_add`/`load`, no new allocation, logging, or syscalls added; tests
  are `#[cfg(test)]`-gated and never compiled into the hot path itself).
  Added a `#[cfg(test)] mod tests` block covering: zeroed-snapshot/
  divide-by-zero guard (`avg_us_per_packet` must be `0.0`, not `NaN`, when
  `packets_in == 0`); independent accumulation of `record_in`/
  `record_out`; `record_in_batch` combining with individual `record_in`
  calls; a `record_in_batch(0, bytes)` case proving the API has no
  invariant tying packet count to byte count (zero-packet batches with
  nonzero bytes are accepted, and the average-guard keys off
  `packets_in`, not `bytes_in`); and `record_processing`'s contribution to
  `avg_us_per_packet`.
  Highest-value case: `counters_wrap_on_u64_overflow_without_panicking`,
  pinning that `AtomicU64::fetch_add` wraps unconditionally on overflow
  (unlike checked arithmetic, atomic fetch-add never panics, even in debug
  builds) — seeded `packets_in`/`bytes_in` at `u64::MAX` via the public
  atomic fields and confirmed `record_in` wraps to `0`/`9` rather than
  aborting the stage. This is the resource-exhaustion/boundary-value
  category from the standing sweep directive: a stage that has processed
  `u64::MAX` packets (astronomically unlikely in practice, but the counter
  type makes no other guarantee) must keep running, not panic the OS
  thread it counts on.
  Did not pursue `elapsed`-dependent branches (`uptime_secs`,
  `packets_per_sec`'s zero-elapsed guard) — `start_instant` is a real
  `std::time::Instant`, not a mockable clock, so forcing `elapsed == 0.0`
  deterministically isn't possible without either a wall-clock sleep-based
  flaky test or introducing a clock abstraction, and the guard clause
  itself is trivial (single comparison, no parsing/adversarial-input
  surface). Left `engine_hls.rs`, `snapshots.rs`, and `pipe_metrics.rs` on
  the candidate list for the next iteration.
- Gates: `scripts/build/resource-limit.sh cargo test --lib stage_metrics`
  — 6/6 new tests pass. `scripts/build/resource-limit.sh cargo clippy
  --lib --benches -- -D warnings` — clean. `cargo fmt --all --check` —
  clean. Pure counter-struct module, not on the Inner Loop table's
  lifecycle-sensitive list and no production code changed — did not
  broaden to `scripts/check/concurrency/contract.sh` or full `cargo test`.
- Commit: `8d166d0d` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed.
- Notes: none.

## 2026-07-18 22:20 HUNT PIPE-METRICS-COUNTER-BOUNDARIES DONE [codex]

- What: continued the low-coverage sweep to `src/media/pipe_metrics.rs`
  (67 lines, zero dedicated tests) — the last small candidate on the list
  after `stage_metrics.rs`. `PipeMetrics` tracks external-transcoder pipe
  back-pressure (stdin write stalls, stdout read idles) with lock-free
  `AtomicU64` counters, structurally identical in shape to
  `stage_metrics.rs`: a `snapshot()` that reads the atomics and computes
  guarded averages. The averages here use `checked_div(...).unwrap_or(0)`
  instead of an `if count > 0` branch, a different-looking but
  equivalent-in-effect divide-by-zero guard worth pinning explicitly.
  Added a `#[cfg(test)] mod tests` block covering: zeroed-snapshot guard
  (`avg_stall_us`/`avg_idle_us` must be `0`, not a panic, when `stalls`/
  `idles` are `0`); independent accumulation of `record_stall`/
  `record_idle`; integer-division truncation for the average (29us over 3
  stalls truncates to 9, not rounds to 10 — `checked_div` is integer
  division, and the snapshot type is `u64`, not `f64`); and the same
  atomic-overflow-wrap case as the `stage_metrics.rs` hunt
  (`counters_wrap_on_u64_overflow_without_panicking`), with an added
  assertion that the `checked_div` guard still fires correctly when
  `stalls` wraps to `0` (avg must read `0`, not divide-by-zero panic or
  stale garbage).
- Gates: `scripts/build/resource-limit.sh cargo test --lib pipe_metrics`
  — 4 new tests plus 2 pre-existing cross-module tests
  (`media::engine::tests::pipe_metrics_snapshot_correctness`,
  `api_runtime_views::telemetry::tests::stage_telemetry_reads_pipe_metrics_from_stage_runtime`)
  all pass (6/6). `scripts/build/resource-limit.sh cargo clippy --lib
  --benches -- -D warnings` — clean. `cargo fmt --all --check` — clean.
  Pure counter-struct module, not on the Inner Loop table's
  lifecycle-sensitive list and no production code changed — did not
  broaden to `scripts/check/concurrency/contract.sh` or full `cargo test`.
- Commit: `68886a10` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed.
- Notes: `engine_hls.rs` and `snapshots.rs` remain on the candidate list
  for the next hunt iteration.

## 2026-07-18 22:40 HUNT ENGINE-HLS-CONSUMER-IDLE-BOUNDARIES DONE [codex]

- What: `src/media/engine_hls.rs` had zero dedicated tests of its own;
  `HlsConsumers::is_idle` only had one flaky-prone real-sleep test in
  `engine_tests.rs`, and the preview registry key prefix functions
  (`hls_preview_registry_key` / `pipeline_id_from_hls_preview_registry_key`)
  had no coverage at all. Added a `#[cfg(test)]` module directly in
  `engine_hls.rs` (matching the in-file test convention used for
  `stage_metrics.rs`/`pipe_metrics.rs`/`srt_stream_id.rs`, since
  `pipeline_id_from_hls_preview_registry_key` is module-private and
  unreachable from `engine_tests.rs`) with 7 deterministic, sleep-free
  tests: registry-key roundtrip (including the case where a pipeline id
  itself contains the `__preview__:` prefix — extraction only strips one
  outer layer, not recursively); rejection of a key where the prefix
  appears mid-string instead of anchored at the start; `is_idle(0)`
  reading idle immediately for a never-touched consumer (no implicit
  startup grace period); a persistent consumer vetoing idle regardless of
  timeout or touch history; and two state-corruption-shaped cases pinning
  the `saturating_sub` guard in `is_idle` — `last_access_ms` artificially
  ahead of `now_ms` must read not-idle rather than underflow/panic, and
  `remove_persistent` called without a matching `add_persistent` wraps
  the `persistent` counter to `u64::MAX` (confirmed via `fetch_sub` with
  no floor) rather than panicking, which then permanently pins the
  consumer as non-idle — a real resource-leak shape if a caller ever
  mismatches add/remove (verified the one production call site in
  `src/lib.rs` already guards this with a `hls_persistent_registered`
  bool, so the wrap is pinned as documented defensive behavior, not
  fixed in production code — consistent with how the atomic-overflow
  wraps in the `stage_metrics.rs`/`pipe_metrics.rs` hunts were pinned
  rather than changed).
- Gates: `scripts/build/resource-limit.sh cargo test --lib engine_hls`
  — 8/8 pass (7 new plus the pre-existing
  `hls_preview_registry_key_roundtrips_through_extraction`-adjacent
  module compiles clean; the original `engine_tests.rs` sleep-based
  `test_hls_consumers_monotonic_idle` is unchanged and still passes).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` + `--check` — clean. Test-only
  change to non-hot-path lifecycle bookkeeping (tokio `RwLock`-guarded
  consumer registry, not a per-packet loop); `engine_hls.rs` is not on
  the Inner Loop table's lifecycle-sensitive file list, so did not
  broaden to `scripts/check/concurrency/contract.sh` or full `cargo
  test`.
- Commit: `f096f3f6` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. The `remove_persistent` unguarded `fetch_sub`
  is worth a defensive `saturating`/`checked` fix if a second call site
  is ever added without the same `*_registered`-bool guard pattern used
  in `src/lib.rs`; not filed as a backlog item since the only current
  caller is already safe.
- Notes: `snapshots.rs` is ruled out (pure data-carrier, no logic).
  Continuing the open-ended scan for the next low-coverage candidate.

## 2026-07-18 23:05 HUNT SRT-QUALITY-COUNTER-BOUNDARIES DONE [codex]

- What: adversarial sweep on `src/media/srt_quality.rs`'s `counter_rate`
  and `quality_from_stats`/`sender_quality_from_stats` logic. The 3
  pre-existing `srt_rates_*` tests in `srt_tests.rs` only covered the
  positive-delta path; the regression guard (`checked_sub` returning
  `None` when a counter goes backward, e.g. across a reconnect that
  reuses the same snapshot struct), the zero-elapsed-seconds guard
  (`elapsed_seconds <= 0.0` short-circuiting before division), and the
  `.max(0)` clamp on signed libsrt counters (guarding against libsrt's
  `-1` "unknown" sentinel sign-extending into a near-`u64::MAX` value
  when cast) were untested. Added 3 tests to the existing
  `srt_tests.rs` sibling file (matching this module's established test
  placement, not a new in-file module): counter regression yields
  `None` instead of a wrapped/huge delta; zero elapsed seconds yields
  `None` instead of inf/NaN; negative sentinel counters on
  `SrtTraceBStats` clamp to `0` in the resulting `PublisherQuality`
  instead of sign-extending.
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  media::srt::tests` — 59/59 pass (56 pre-existing plus 3 new; the
  correct module path, since `srt_tests.rs` is pulled in via `#[path =
  "srt_tests.rs"] mod tests;` inside `srt.rs`, not a standalone
  `srt_tests` target). `scripts/build/resource-limit.sh cargo clippy
  --lib --benches -- -D warnings` — clean. `cargo fmt --all` + `--check`
  — clean (fmt collapsed one new test's call onto fewer lines;
  confirmed cosmetic via diff review, no semantic change). Test-only
  change to non-hot-path quality-reporting arithmetic (not a per-packet
  loop); did not broaden to `scripts/check/concurrency/contract.sh` or
  full `cargo test`.
- Commit: `da946c19` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. `counter_rate` and the `.max(0)` clamps
  already guard correctly in production code; this hunt only closed
  test-coverage gaps, no behavior change needed.
- Notes: continuing the open-ended scan of `src/media/` for the next
  low-coverage candidate.

## 2026-07-18 23:30 HUNT SRT-MUXER-SHARD-POOL-BOUNDARIES DONE [codex]

- What: adversarial sweep on `SrtMuxerShardPool` in
  `src/media/engine_registries.rs` — the least-occupancy shard
  load-balancer backing SRT egress muxer sharding. Only one test
  existed (retiring-shard reuse gating). Added 9 tests covering:
  idempotent re-assign of the same (output_id, attempt_id) pair does
  not double-occupy its shard; the idempotent-return `overflowed` flag
  uses strict `>` against capacity, so sitting exactly at capacity is
  not flagged as overflowed (a real boundary in the existing code, not
  a bug — pinned as documented behavior); a reconnect (same output_id,
  new attempt_id) releases the stale assignment via the internal
  `release_assignment(..., retire_empty_shard=false)` path and the
  freed shard is immediately reusable without waiting on
  `finish_retiring`; least-occupied shard selection is deterministic
  under ties (`min_by_key` returns the first minimal element); the
  overflow-warn flag fires exactly once across repeated overflow
  assigns once both shard count and per-shard capacity are exhausted;
  a stale `release` carrying a superseded `attempt_id` is a no-op that
  cannot evict the current assignment (guards a cleanup-task-races-a-
  reconnect scenario); releasing an unknown `output_id` and retiring an
  unknown shard index are both no-ops rather than panics; and the
  `max_shards`/`max_outputs_per_shard` `debug_assert` invariants are
  enforced (`#[should_panic]`, debug/test-build only — noted as a
  release-mode gap below).
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  engine_registries` — 11/11 pass (2 pre-existing plus 9 new).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` + `--check` — clean. Test-only
  change to in-memory load-balancing bookkeeping (no sockets, no
  syscalls); did not broaden to `scripts/check/concurrency/contract.sh`
  or full `cargo test`.
- Commit: `0aac54e9` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed as a fix. Noted but not changed: `assign`'s
  final overflow-fallback arm (`push(0); 0`) is unreachable when the
  `debug_assert!(max_shards > 0)` holds, but `debug_assert!` compiles
  out in release builds — calling `assign` with `max_shards == 0` in a
  release binary would silently create a shard despite the caller's
  stated cap instead of panicking. No production call site currently
  passes a non-constant/zero `max_shards`, so this is a latent
  invalid-assumptions gap, not a live bug; worth a `debug_assert_eq!`
  upgrade to a real guard only if a call site ever derives `max_shards`
  from configuration.
- Notes: continuing the open-ended scan of `src/media/` for the next
  low-coverage candidate.

## 2026-07-18 23:50 HUNT SRT-POLICY-FALLBACK-SEMANTICS DONE [codex]

- What: adversarial hunt on `src/media/srt_policy.rs`'s
  `build_policy_snapshot`, which had only one existing test (covering the
  double-failure case where both the per-entry policy and the global
  fallback fail to resolve). Reading `src/media/srt/config.rs` and
  `src/domain/srt_ingest.rs::resolve()` surfaced a genuine, previously
  undocumented asymmetry: a malformed `serialized_policy` JSON string and a
  wholly absent (`None`) one are indistinguishable — both collapse to
  `SrtPipelineIngestConfig::default()` (mode = Inherit) via
  `parse_pipeline_srt_ingest_policy(...).unwrap_or_default()`, and Inherit
  silently resolves through to whatever the global policy currently is,
  with **no warning logged**. This differs from the separate `Err` branch
  (a parseable-but-invalid policy that fails `.resolve(&global)`), which
  does `warn!` before falling back. Added 6 tests: a corrupted persisted
  policy and a genuinely-absent one both silently inherit the global
  policy, pinning the no-warning-either-way behavior; a parseable-but-
  invalid per-entry policy (short passphrase) successfully falls back to a
  *valid* global (the fallback-succeeds branch, previously uncovered —
  the existing test only exercised fallback-also-fails); duplicate
  `stream_key` entries across two pipelines, confirming last-insert-wins
  via `HashMap::insert` overwrite; an empty `entries` slice producing an
  empty snapshot without panicking; and `replace()` atomically swapping
  snapshots so stream keys absent from the new entry list stop resolving.
- Gates: `scripts/build/resource-limit.sh cargo test --lib srt_policy` —
  6/6 pass (1 pre-existing plus 5 new). `scripts/build/resource-limit.sh
  cargo clippy --lib --benches -- -D warnings` — clean. `cargo fmt --all`
  + `--check` — clean. Test-only change to in-memory policy resolution
  (no sockets, no syscalls); did not broaden to
  `scripts/check/concurrency/contract.sh` or full `cargo test`.
- Commit: `1e22f998` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed as a fix. The silent malformed-JSON-equals-absent
  behavior is now pinned by test rather than changed — flagging that a
  corrupted persisted policy for a pipeline that was meant to be
  encrypted-only would silently downgrade to whatever the global mode is
  (e.g. plaintext), with zero operator-visible diagnostic. Worth revisiting
  as a real fix (log a warning on JSON parse failure, distinct from
  absence) only as a deliberate follow-up, not as a side effect of this
  test sweep.
- Notes: continuing the open-ended scan of `src/media/` for the next
  low-coverage candidate (`ts_chunk_ring.rs` 198 lines/2 tests looks like
  the next best ratio).

## 2026-07-19 00:10 HUNT TRANSCODE-PROFILE-VALIDATION-BOUNDARIES DONE [codex]

- What: `ts_chunk_ring.rs` was re-evaluated and deprioritized — its 4
  existing tests already cover its own thin-wrapper logic, and the
  substantive ring behavior lives in already-loom-tested
  `ring_buffer.rs`. Re-ran a lines-per-test ranking across all
  `src/media/*.rs` files, cross-referenced against `#[path]`/`mod tests`
  siblings to filter out false positives (`engine.rs`, `srt.rs`,
  `rtmp.rs`, `mpegts.rs`, `ring_buffer.rs`, `srt_egress.rs`,
  `external_transcoder.rs` all have coverage via included sibling test
  files), then cross-checked the remaining candidates
  (`stage_registry_access.rs`, `snapshots.rs`, `ingest_auth.rs`) against
  this journal and confirmed all three were already ruled out in an
  earlier iteration (see the STAGE-METRICS and ENGINE-HLS entries above:
  `ingest_auth.rs` is pure trait/type definitions with no branching,
  `stage_registry_access.rs` is thin CRUD glue already covered
  transitively via `engine_stage_tests.rs`, `snapshots.rs` is a pure data
  carrier). With `src/media/` effectively exhausted of small, self-
  contained, untested candidates, broadened the scan to the rest of
  `src/` per the standing directive's open-ended scope. Found
  `src/domain/transcode_profile.rs` (121 lines): `TranscodeProfile::
  validate()` has real whitelist/range-boundary logic (preset and tune
  exact-match whitelists, an inclusive `0..=51` crf range) with **zero**
  dedicated tests anywhere in the codebase — the only prior reference was
  a single `assert!(TranscodeProfile::default().validate().is_ok())`
  line inside `media/profiles.rs`'s test module, which never exercised
  the error paths at all. Added a `#[cfg(test)] mod tests` block
  covering: every documented preset/tune value validates; an unknown
  preset/tune is rejected; preset matching is exact-case (`"Ultrafast"`
  capitalized is rejected, pinning that there is no case-insensitive
  fallback); crf boundary inclusivity at 0 and 51; crf rejection just
  outside the range and at `i32::MIN`/`i32::MAX` (proving the range
  check itself never overflows/panics at the type's extremes); and a
  pinning test that `validate()` does **not** bound `bitrate`,
  `max_bitrate`, `gop`, `bframes`, `width`, or `height` at all — a
  profile with negative bitrate, zero gop, and `u32::MAX` dimensions
  still validates `Ok`, since those fields rely on a `0 = use
  source/no limit` sentinel convention enforced by callers, not by this
  type. Also added serde coverage: an empty `{}` object fills every
  field from its documented default; a negative bitrate deserializes
  without any validation being applied at parse time (validation is a
  separate, opt-in step callers must call); and unknown JSON fields are
  silently ignored rather than rejected (no `#[serde(deny_unknown_fields)]`).
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  transcode_profile` — 15/15 pass (13 new plus 2 pre-existing
  `application::transcode_profiles::tests` in the same filter,
  unaffected). `scripts/build/resource-limit.sh cargo clippy --lib
  --benches -- -D warnings` — clean. `cargo fmt --all` + `--check` —
  clean (auto-reformatted the new block's multi-line `assert!` calls,
  cosmetic only). Pure domain value-type module, not on the Inner Loop
  table's lifecycle-sensitive file list and no production code changed —
  did not broaden to `scripts/check/concurrency/contract.sh` or full
  `cargo test`.
- Commit: `51a69cf9` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. The lack of bitrate/gop/dimension bounds in
  `validate()` is pinned as documented current behavior, not flagged as a
  bug — callers depend on `0` as a sentinel and no caller currently
  passes unchecked user input for those fields without an upstream
  bound; a stricter `validate()` would be a deliberate API-contract
  change, not an in-scope fix for a test sweep.
- Notes: `src/media/` is now largely exhausted of small, self-contained,
  untested candidates. Continuing the open-ended scan into `src/domain/`,
  `src/application/`, and `src/api_runtime_views/`, which have not yet
  been swept this session; `src/bin/test_harness/` is deliberately
  excluded from this scan since it is harness tooling validated by its
  own `correctness*`/live-mode gates, not application logic.

## 2026-07-19 00:35 HUNT API-VIEW-MODELS-FORMATTING-HELPERS DONE [codex]

- Scope: continued the open-ended adversarial scan into
  `src/api_view_models.rs`. Ruled out two other candidates first without
  writing code: `src/api_runtime_views/graph.rs` (thin async orchestration
  over live `MediaEngine` `RwLock`-guarded state; would need heavy engine
  mocking to unit-test in isolation and its real logic is already exercised
  transitively by existing integration coverage, same reasoning as the
  earlier-ruled-out `stage_registry_access.rs`) and `src/application/ports.rs`
  (pure trait/type-definition boilerplate with no branching logic, same
  reasoning as the earlier-ruled-out `ingest_auth.rs`).
- Finding: `human_bytes`, `human_duration_ms`, and
  `srt_recv_buffer_occupancy` in `src/api_view_models.rs` are private pure
  functions reachable from HTTP-facing JSON response bodies (via
  `processing_graph_ingest_details` and related call sites) with zero test
  coverage. Added 13 new `#[test]` functions to the existing `mod tests`
  block covering: `human_bytes` byte/KiB/MiB tier boundaries at 1023/1024
  and `1024*1024`, no-panic behavior at `u64::MAX` (confirming there is no
  GiB/TiB tier — huge values just render as a large MiB number);
  `human_duration_ms` ms/s/min tier boundaries at 999/1000 and 60_000,
  no-panic behavior at `u64::MAX` (no hour tier); `srt_recv_buffer_occupancy`
  returning `None` when either `Option<i32>` field is `None`, returning
  `None` when both resolve to a total of 0 (div-by-zero guard), clamping
  negative `i32` values (libsrt reports `-1` for "unavailable") to 0 via
  `.max(0)` before the `u64` cast rather than underflowing, a normal
  percentage computation, and an `i32::MAX` case proving the
  `u64` intermediate cast avoids overflow that would occur if the
  multiplication/sum stayed in `i32`.
- Pinned quirk: both `human_bytes` and `human_duration_ms` select their
  display tier with an `if raw_value < threshold` check on the *unrounded*
  input, then independently format the *scaled* value at one decimal place.
  A value one unit below the threshold can round up to look like it already
  crossed it: `human_bytes(1024 * 1024 - 1)` renders `"1024.0 KiB"` instead
  of bumping to MiB, and `human_duration_ms(59_999)` renders `"60.0 s"`
  instead of bumping to minutes. Not a panic or data-loss bug — pinned with
  an explanatory test comment rather than "fixed", per this session's
  pin-don't-fix convention; changing the tier-selection logic to round-then-
  compare would be a deliberate, human-reviewed behavior change to
  operator-facing display text, out of scope for a test sweep.
- Gates: `scripts/build/resource-limit.sh cargo test --lib api_view_models`
  — 30/30 pass (15 new plus 15 pre-existing in the same filter, unaffected).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` + `--check` — clean, no reformatting
  needed. Test-only change to a module already on the frontend/backend
  contract surface for its JSON-shaping functions, but no production code
  or JSON shape changed — did not run `scripts/check/api-contract.sh`
  since no wire-format behavior was touched, only new tests added.
- Commit: `ec60433b` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. The tier-selection/rounding disagreement is
  pinned as documented current display behavior, not flagged as a bug.
- Notes: continuing the open-ended scan. Remaining unswept areas: most of
  `src/application/` (`ingest.rs`, `reconcile.rs`, `egress.rs`,
  `hls_preview.rs`, `recording.rs`, `srt_ingest.rs`, `ingest_security.rs`,
  `models.rs`, `settings.rs`, `graph.rs` — most already have some tests,
  ratios not yet deeply evaluated) and `src/api_runtime_views/` thin-ratio
  files (`status.rs` 833 lines/5 tests, `resource_map.rs` 736/1,
  `telemetry.rs` 403/3) worth a closer look next.

## 2026-07-19 01:00 HUNT RESOURCE-MAP-JSON-SHAPING-HELPERS DONE [codex]

- Scope: continued the open-ended scan into `src/api_runtime_views/`.
  Re-checked `telemetry.rs` first: an earlier ratio scan undercounted it
  (grepping only `#[test]` and missing `#[tokio::test]`) — it actually has
  3 solid `#[tokio::test]` integration-style tests covering its async
  `MediaEngine`-coupled functions, including a real regression case
  (`telemetry_reads_runtime_stage_after_metrics_side_map_removed`). Ruled
  it out as already adequately covered for its shape (thin async glue,
  same category as the earlier-ruled-out `graph.rs`).
- Finding: `src/api_runtime_views/resource_map.rs` (736 lines) had only one
  `#[test]`, despite containing roughly 15 pure, synchronous functions that
  shape the operator/agent-facing `GET .../resource-map` JSON from
  untrusted-shaped `serde_json::Value` telemetry snapshots — field
  extraction, group-key/label derivation, thread/hotspot merging, node
  scoring, sorting/truncation, and per-node-kind builders. Added 16 new
  `#[test]` functions to the existing `mod tests` block covering:
  `ResourceMapOptions::new`'s `top_n` clamping at the low end (`Some(0)`
  clamps up to 1, not an empty view) and high end (`Some(MAX_TOP_N +
  1000)` clamps down, guarding against unbounded node allocation from a
  malicious or buggy query parameter); `number_field` returning 0 (not
  panicking or wrapping) for a missing key, a non-integer string, a
  negative number, and a fractional number, plus round-tripping
  `u64::MAX`; `group_key`/`group_label` defaulting cleanly on missing
  `kind`/`execution`/`label` fields, including an all-whitespace egress
  label (no first word, falls back to `"unknown"`) and an empty group key
  (single empty split segment, falls through to the `other` arm without
  panicking); `merge_thread_counts` accumulating counts across repeated
  calls while silently skipping non-numeric thread-count entries;
  `append_hotspots` deduplicating repeated hotspot strings while ignoring
  non-string array entries; `queue_hotspots`' 75%-of-capacity threshold
  boundary (`len*100 >= capacity*75`, inclusive at exactly 75%) and its
  `capacity > 0` guard, which prevents a zero-capacity queue from
  reporting `queue_high` even at `u64::MAX` length (the multiplication
  would otherwise saturate to a false positive without the guard);
  `execution_for_stage`'s full backend-string-to-execution-model mapping
  including both case variants seen in the wild (`externalFfmpeg` /
  `ExternalFfmpeg`) and its default-to-`shared` fallback for an unknown or
  missing backend; `stage_backend_pid` rejecting a `backendPid` value that
  does not fit in `u32` (e.g. `u64::MAX`) via `None` rather than silently
  truncating it to a different, wrong pid; `node_score`'s cpu-dominates-
  memory weighting (1% CPU outweighs just under 1 MiB of memory, by
  design); `top_nodes` sorting descending by score with correct
  truncation, including a truncate-to-zero case that must return empty
  rather than panic; and `egress_node`/`source_ring_node`'s protocol- and
  payload-driven branches (SRT egress is the only protocol treated as an
  app-owned OS thread; a source ring only reports the `retained_payload`
  hotspot when it actually holds bytes).
- Gates: `scripts/build/resource-limit.sh cargo test --lib resource_map`
  — 17/17 pass (16 new plus 1 pre-existing, unaffected).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` + `--check` — clean (auto-wrapped
  a few of the new multi-line `assert_eq!` calls, cosmetic only, re-ran
  the test suite afterward to confirm no behavior changed). Test-only
  change; the JSON shape of `resource_map`'s public output was not
  touched, so did not run `scripts/check/api-contract.sh`.
- Commit: `ef795dac` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed. All observed defaults/clamps/guards are
  existing, intentional-looking behavior; nothing found here rose to the
  level of a bug.
- Notes: continuing the open-ended scan. Remaining unswept areas: most of
  `src/application/` (`ingest.rs`, `reconcile.rs`, `egress.rs`,
  `hls_preview.rs`, `recording.rs`, `srt_ingest.rs`, `ingest_security.rs`,
  `models.rs`, `settings.rs`, `graph.rs`) and `src/api_runtime_views/`
  `status.rs` (833 lines/5 tests) worth a closer look next.

## 2026-07-19 01:20 HUNT STATUS-CPU-AFFINITY-OVERFLOW FIXED [codex]

- Scope: continued into `src/api_runtime_views/status.rs` (833 lines, 5
  existing tests). Most of the file is `output_status`/`health_snapshot`/
  `health_summary_snapshot`, thin async orchestration over live
  `MediaEngine` registries (same shape as the already-ruled-out
  `telemetry.rs`/`graph.rs`), already covered by two existing lock-ordering
  regression tests. The file's genuinely undertested surface was its small
  set of pure, synchronous JSON/parsing helpers: `parse_cpu_list_count`,
  `parse_cgroup_cpu_max`, and `host_setting_json`.
- Finding (real bug, not just a gap): `parse_cpu_list_count` computed a
  CPU-range length as `end - start + 1` with plain (non-checked)
  arithmetic. `end >= start` was already guaranteed by an earlier check, so
  the subtraction itself was safe, but the trailing `+ 1` was not: for a
  range ending at or near `u64::MAX` (e.g. `"0-18446744073709551615"`),
  `u64::MAX + 1` overflows. Verified with a standalone repro compiled both
  with and without `debug-assertions`: it panics ("attempt to add with
  overflow") under debug-assertions — the mode `cargo test` builds in by
  default — and silently wraps to `Some(0)` in release. In production this
  parses the kernel-reported `Cpus_allowed_list` from `/proc/self/status`
  during health-settings reporting, so real-world exploitability is low
  (the kernel is very unlikely to report a range spanning 2^64 CPUs), but
  the function has no other input validation boundary and must not panic
  or silently corrupt its result on any string. Fixed by computing the
  range length as `(end - start).checked_add(1)?` before folding it into
  the running `checked_add` total, so an unrepresentable range length now
  returns `None` (parse failure) instead of panicking or wrapping.
- Added regression/adversarial coverage: a test pinning the fixed overflow
  case (`"0-18446744073709551615"` → `None`, not a panic); empty-string and
  single-element-range (`"5-5"` → `Some(1)`) cases that were previously
  unexercised boundaries of the same parser; and three new tests for
  `host_setting_json`'s status derivation — the `current >= required`
  threshold is inclusive at exactly `required` (`"ok"` at the boundary,
  `"warning"` one below it), a missing `current` reading reports a distinct
  `"unknown"` status rather than being conflated with `"warning"` (checked
  against `required = u64::MAX` to rule out any accidental comparison
  against the sentinel), and a `None` detail passes through as JSON `null`
  rather than being coerced to an empty string or omitted.
- Gates: `scripts/build/resource-limit.sh cargo test --lib status::` —
  10/10 pass (5 new plus 5 pre-existing) both before and after `cargo fmt
  --all`. `scripts/build/resource-limit.sh cargo clippy --lib --benches --
  -D warnings` — clean. `scripts/check/api-contract.sh` — 109/109 contract
  tests plus the `api-smoke` end-to-end script pass (run because the file
  lives under `src/api_runtime_views/`, even though this change only
  touched an internal helper and test-only code, not any public JSON
  shape).
- Commit: `51bd88a0` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — the overflow was fixed directly in this commit
  since it was a small, local, off-hot-path arithmetic correction with an
  obvious safe fix, not a design question needing a separate backlog item.
- Notes: continuing the open-ended scan. Remaining unswept areas: most of
  `src/application/` (`ingest.rs`, `reconcile.rs`, `egress.rs`,
  `hls_preview.rs`, `recording.rs`, `srt_ingest.rs`, `ingest_security.rs`,
  `models.rs`, `settings.rs`, `graph.rs`) — none of these have been ratio-
  scanned yet with the corrected `#[(tokio::)?test]` counting method.

## 2026-07-19 02:00 HUNT HLS-PREVIEW-CODEC-LEVEL-DEFAULT FIXED [codex]

- Scope: checked `src/application/graph.rs` (106 lines, 1 existing test)
  first and ruled it out — it is thin delegation
  (`desired_pipeline_graphs`/`planned_output` route to
  `crate::planner::graph_plan` based on one branch,
  `OutputUrlScheme::from_url(&output.url).is_hls_family()`, already covered
  by the file's one existing test) with no further hunt value. Moved on to
  `src/application/hls_preview.rs` (516 lines, 2 existing tests, both
  `#[tokio::test]`-level orchestration checks). Most of the file is thin
  async glue over `Arc<MediaEngine>` (`ensure_hls_preview`,
  `primary_playlist`, `*_segment`/`*_playlist` handlers — same
  already-ruled-out shape as `telemetry.rs`/`graph.rs`/`status.rs`'s async
  handlers), but it also holds ~20 pure, synchronous HLS playlist and
  H.264/HEVC codec-string-building functions with zero direct unit
  coverage: `quote_hls_attr`, `build_hls_master_playlist`,
  `build_hls_audio_track_name`, `estimate_hls_master_bandwidth`,
  `estimate_audio_bandwidth`, `build_hls_codec_list`,
  `build_hls_video_codec`, `build_h264_codec_string`,
  `estimate_h264_level_idc`, `parse_h264_level_idc`,
  `build_hevc_codec_string`, `parse_h265_level_tenths`,
  `build_hls_audio_codec`.
- Finding (real bug, not just a gap): `build_hevc_codec_string`'s fallback
  for a missing/unparseable `level` was `level_tenths.unwrap_or(120)`, but
  `level_tenths` is in "major*10+minor" units (`parse_h265_level_tenths`
  returns `40` for level "4.0"), and gets multiplied by 3 afterward to
  produce the actual `general_level_idc` written into the codec string.
  The `120` fallback was itself already expressed in
  `general_level_idc`-ish units, so the `*3` ran a second time on it,
  producing `L360` — past the HEVC spec's max level (6.2, `L186`) — for
  any stream with a missing or malformed level string, instead of a sane
  default around level 4.0 (`L120`). Cross-checked the two producers of
  `VideoMeta.level` (`src/media/rtmp/flv.rs:42` and
  `src/media/mpegts_probe.rs:441`, both format "major.minor" from a
  decoder-reported `level_idc`) to confirm the unit `parse_h265_level_tenths`
  expects and produces, then fixed the fallback to `40` (level 4.0),
  matching those units.
- Added adversarial/regression coverage (43 new tests): H.264/HEVC level
  parsing (`None`/empty/whitespace input, missing-dot single-value levels,
  extra dot segments, non-numeric parts, major overflowing `u8`, and the
  `saturating_mul`/`saturating_add` boundary at `"99.9"` clamping to
  `u8::MAX` instead of wrapping); `estimate_h264_level_idc`'s three-tier
  boundary conditions (exact-`216_000`/`108_000` macroblocks-per-second
  thresholds) and its zero-dimension/non-finite-fps guards; a dedicated
  regression test for the `build_hevc_codec_string` fallback-unit bug
  above (`level: None` and `level: Some("garbage")` both now producing
  `L120`, not `L360`); `estimate_hls_master_bandwidth`'s non-finite/
  non-positive video-bandwidth filtering, its `.max(1)` zero-bandwidth
  floor (a `bw` that rounds down to `0` must still report `1`), and a
  `saturating_add` overflow check at `f64::MAX`; `estimate_audio_bandwidth`'s
  full codec/channel-tier lookup table plus case-insensitive codec
  matching; `build_hls_codec_list`'s dedup behavior and empty-input `None`
  case; `quote_hls_attr`'s escape-backslash-before-quote ordering (would
  double-escape if reversed); `build_hls_audio_track_name`'s
  title/language whitespace-trim fallback chain; and two
  `build_hls_master_playlist` integration checks — a track title
  containing an embedded `"` round-trips through `NAME=` correctly escaped
  (an M3U8-injection angle for attacker-influenced stream metadata), and
  `DEFAULT=YES` is set on exactly one audio track (ordinal 0) regardless
  of track count.
- Gates: `scripts/build/resource-limit.sh cargo test --lib hls_preview::`
  — 45/45 pass (43 new plus 2 pre-existing).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` — no changes beyond the new tests
  (diff stat matched exactly); `cargo fmt --all --check` — clean.
- Commit: `69319407` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — the level-default bug was fixed directly in
  this commit (a small, local, off-hot-path constant correction with an
  obvious fix), consistent with the "fix real bugs directly" convention
  used for the `status.rs` overflow fix earlier in this session.
- Notes: continuing the open-ended scan. Remaining unswept areas in
  `src/application/`: `ingest.rs` (923/12), `reconcile.rs` (648/12),
  `egress.rs` (549/7), `recording.rs` (470/8), `srt_ingest.rs` (357/5),
  `ingest_security.rs` (195/5), `models.rs` (156/2),
  `transcode_profiles.rs` (151/2, distinct from the already-hunted
  `src/domain/transcode_profile.rs`), `settings.rs` (140/2) — none
  ratio-scanned in depth yet beyond raw line/test counts.

## 2026-07-19 02:20 HUNT INGEST-SECURITY-VALIDATE-BRANCHES DONE [codex]

- Scope: checked `src/application/ingest_security.rs` (195 lines, 5
  existing tests) and ruled it out — its own doc comment states it owns
  only "JSON round-tripping" between `MetaStore` and the domain config,
  with "validation semantics" living in `crate::domain::ingest_security`.
  The 5 existing tests already cover the full state space for that thin
  glue: valid-JSON load, malformed/store-error fallback to defaults,
  normalize-on-load, save round-trip, and save-with-normalize. Same
  pattern as the already-ruled-out `graph.rs`/`telemetry.rs`/`ports.rs`.
  Moved to the real target named in that doc comment:
  `src/domain/ingest_security.rs` (88 lines, 2 existing tests) — small,
  but its `validate()` had only one of four field branches exercised
  (`failure_limit`), no test that a valid config passes, no boundary
  test at exactly `1`, and `normalize()`'s `.max(1)` clamp had no
  `i64::MIN` case despite taking `i64` input from arbitrary
  deserialized/API-supplied JSON.
- Finding: no bug — `.max(1)` on `i64` cannot overflow (`i64::MIN.max(1)
  == 1`), confirmed by the new `normalize_clamps_i64_min_without_overflow`
  test. This is a coverage gap, not a defect.
- Added regression/adversarial coverage (14 new tests, 2 pre-existing
  kept): one `validate()` test per remaining field
  (`failure_window_ms`/`ban_ms`/`tracked_ip_limit`, each pinning its own
  distinct error string), a negative-value case for `failure_limit`
  (previously only `0` was tested), a default-config-passes-validate
  case, an all-fields-set-to-`1` boundary-passes case, an
  all-fields-zero case pinning that the first field in declared order
  wins (documents `validate`'s short-circuit order), an
  already-valid-values-are-unchanged case for `normalize` (previously
  only the clamping path was tested), an exactly-`1`-boundary
  no-op case, the `i64::MIN` overflow-safety case above, and an
  integration check that `normalize()` then `validate()` always
  succeeds regardless of how invalid the input was
  (`i64::MIN`/`0`/`-42`/`i64::MIN` all clamp to a config that validates
  clean).
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  ingest_security::` — 18/18 pass (16 new/expanded plus 2 pre-existing,
  spanning both the `application::` and `domain::` modules of the same
  name). `scripts/build/resource-limit.sh cargo clippy --lib --benches
  -- -D warnings` — clean. `cargo fmt --all` — no changes beyond the new
  tests; `cargo fmt --all --check` — clean.
- Commit: `2387037a` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept areas in
  `src/application/`: `ingest.rs` (923/12), `reconcile.rs` (648/12),
  `egress.rs` (549/7), `recording.rs` (470/8), `srt_ingest.rs` (357/5),
  `models.rs` (156/2, checked — `JobStatus` string round-trip already
  fully covered, thin serde records otherwise, ruling out),
  `transcode_profiles.rs` (151/2, checked — thin persistence glue over
  `crate::domain::transcode_profile`, already-hunted per the prior
  `TRANSCODE-PROFILE-VALIDATION-BOUNDARIES` entry, ruling out),
  `settings.rs` (140/2) — not yet ratio-scanned in depth.

## 2026-07-19 02:35 HUNT SETTINGS-BACKEND-POLICY-FALLBACK DONE [codex]

- Scope: `src/application/settings.rs` (140 lines, 2 existing tests).
  `load_settings_snapshot` is thin cross-source orchestration delegating
  to already-hunted/well-tested loaders (`load_recording_settings`,
  `load_global_srt_ingest_config`, `crate::media::profiles::
  current_effective`, `IngestSecurityService::get_config`), already
  covered end-to-end by its one integration test. The undertested
  surface was `load_backend_policy`'s own fallback chain
  (`.ok().flatten().and_then(...).unwrap_or(default_policy)`): only the
  "valid persisted JSON present" branch had a test; the "nothing
  persisted," "persisted value isn't valid JSON," and "persisted JSON
  parses but isn't the right shape" branches were all unexercised.
- Finding: no bug — all three fallback branches correctly resolve to the
  caller-supplied default, exactly as the `.ok()`/`.flatten()`/
  `.and_then()` chain implies. Coverage gap, not a defect.
- Added regression coverage (3 new tests): no meta row for the policy
  key at all; a meta row present but containing invalid JSON syntax
  (`"{not valid json"`); and a meta row present with syntactically valid
  but wrong-shaped JSON (a JSON array instead of the expected object) —
  all three assert the result equals the caller's `default_policy`
  rather than partially applying or panicking.
- Gates: `scripts/build/resource-limit.sh cargo test --lib settings::`
  — 10/10 pass (3 new plus 2 pre-existing in `application::settings`,
  plus 5 unrelated pre-existing `api::settings` tests sharing the same
  module-name filter). `scripts/build/resource-limit.sh cargo clippy
  --lib --benches -- -D warnings` — clean. `cargo fmt --all` — no
  changes beyond the new tests; `cargo fmt --all --check` — clean.
- Commit: `df977f2d` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept areas in
  `src/application/`, largest-first: `ingest.rs` (923/12),
  `reconcile.rs` (648/12), `egress.rs` (549/7), `recording.rs` (470/8),
  `srt_ingest.rs` (357/5). All files under ~200 lines in
  `src/application/` have now been ratio-scanned and either hunted or
  ruled out this session.

## 2026-07-19 02:50 HUNT SRT-INGEST-APPCONFIG-FALLBACK DONE [codex]

- Scope: `src/application/srt_ingest.rs` (357 lines, 5 existing tests).
  `load_policy_store`/`refresh_policy_store` are thin orchestration
  wired together from `load_global_srt_ingest_config` plus catalog
  lookups, already covered end-to-end by their own two integration
  tests. `load_global_srt_ingest_config`'s meta-store-present and
  fail-closed-vs-fail-open validation branches were also already
  covered by 3 existing tests. The gap was the private
  `srt_global_config_from_appconfig` helper (lines 76-89) and its
  `.or_else(...)` integration at line 24: no existing test ever left
  the fake meta store empty (`value: None`) or passed a non-`None`
  `srt_passphrase` argument, so the entire app-config-derived fallback
  path — used when no SRT ingest config has ever been persisted to the
  meta store yet an operator has configured a global passphrase via
  app config/CLI flags — was completely dead code as far as the test
  suite was concerned.
- Finding: no bug — the fallback and its priority ordering (meta store
  wins when present, even over a non-empty app-config passphrase)
  behave exactly as implied by the `.or_else()`/`.unwrap_or_default()`
  chain. Coverage gap, not a defect.
- Added regression coverage (6 new tests): 3 direct unit tests of
  `srt_global_config_from_appconfig` (`None` passphrase → `None`;
  empty-string passphrase → `None`, i.e. treated as absent rather than
  a valid empty secret; a real passphrase → an `Encrypted` config
  carrying the passphrase and `pbkeylen` through unchanged) plus 3
  integration tests through `load_global_srt_ingest_config` (empty meta
  store falls back to the app-config passphrase; empty meta store and
  no app-config passphrase falls back to `SrtGlobalIngestConfig::
  default()`; a present-but-plaintext meta-store value wins over a
  simultaneously-supplied app-config passphrase, pinning the priority
  order).
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  srt_ingest::` — 16/16 pass (11 in `application::srt_ingest`, 6 new
  plus 5 pre-existing; plus 5 unrelated pre-existing
  `domain::srt_ingest` tests sharing the module-name filter).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` — no changes beyond the new
  tests; `cargo fmt --all --check` — clean.
- Commit: `58063998` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept areas in
  `src/application/`, largest-first: `ingest.rs` (923/12),
  `reconcile.rs` (648/12), `egress.rs` (549/7), `recording.rs` (470/8).
  All files under ~400 lines in `src/application/` have now been
  ratio-scanned and either hunted or ruled out this session.

## 2026-07-19 03:05 HUNT RECORDING-SETTINGS-FALLBACK-AND-SHORT-CIRCUIT DONE [codex]

- Scope: `src/application/recording.rs` (470 lines, 8 existing tests).
  `recording_enabled_meta_key`, `load_recording_enabled`,
  `load_recording_enabled_map`, and `save_recording_settings` were
  already fully covered. `spawn_recording_task` and
  `apply_recording_commands`'s start/stop dispatch are exercised by
  live-`MediaEngine` lifecycle tests, matching the already-ruled-out
  "thin async glue" pattern for their happy-path behavior. Two gaps
  remained: `load_recording_settings`'s malformed-JSON fallback branch
  had no test (only the "key missing" fallback case existed), and
  `apply_recording_commands`'s `needs_settings` short-circuit — which
  skips loading `RecordingSettings` entirely when the command batch
  contains no `RecordingCommand::Start` — had no test proving the skip
  actually happens rather than just being incidentally harmless.
- Finding: no bug — `load_recording_settings` falls back to
  `RecordingSettings::default()` on malformed JSON exactly like the
  missing-key case, and `needs_settings` correctly avoids the
  meta-store call for stop-only batches. Coverage gap, not a defect.
- Added regression coverage (2 new tests):
  `load_recording_settings_falls_back_to_default_on_malformed_json`
  pins the malformed-JSON fallback. `apply_recording_commands_skips_settings_load_when_only_stopping`
  uses a call-counting `MetaStore` fake (`get_meta` always errors and
  increments an `AtomicUsize`) dispatched with a `Stop`-only command
  list, asserting zero `get_meta` calls — proving the short-circuit
  actually elides the load rather than merely tolerating a failed one.
- Gates: `scripts/build/resource-limit.sh cargo test --lib recording::`
  — 39/39 pass (10 in `application::recording`: 2 new plus 8
  pre-existing; remainder pre-existing `media::recording` tests sharing
  the module-name filter). `scripts/build/resource-limit.sh cargo
  clippy --lib --benches -- -D warnings` — clean. `cargo fmt --all` —
  no changes beyond the new tests; `cargo fmt --all --check` — clean.
  `scripts/check/concurrency/fast.sh` — 135/135 pass (run per the
  Inner Loop table's lifecycle-file gate for `recording.rs`).
- Commit: `4956018a` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept
  `src/application/` files: `ingest.rs` (923/12), `reconcile.rs`
  (648/12), `egress.rs` (549/7).

## 2026-07-19 03:30 HUNT INGEST-AUTH-ASYMMETRY-AND-FILE-INGEST-GAPS DONE [codex]

- Scope: `src/application/ingest.rs` (923 lines, 12 existing tests) —
  the largest remaining unswept `src/application/` file. Covers
  `PipelineStoreIngestAuthenticator`/`PipelineAccessAuthenticator`
  dispatch on `PipelineAccessMode`, the shared
  `authenticate_stream_key_for_scope` core auth logic, and the
  file-ingest lifecycle helpers (`resolve_file_ingest_context`,
  `load_pipeline_file_ingest_state`, `clear_stream_key_file_ingests`,
  `persist_pipeline_file_ingest`, `remove_pipeline_file_ingest`).
- Finding: no bug — five coverage gaps, all confirmed correct as
  designed. (1) `authenticate_publish_stream_key` (RTMP) and
  `authenticate_srt_stream_key` share `authenticate_stream_key_for_scope`
  but pass different `clear_on_success` booleans (`false` vs `true`);
  a successful RTMP publish auth intentionally does NOT clear a prior
  failure count, while SRT auth does. Only the SRT (clearing) side had
  a test; the RTMP (non-clearing) side was unproven, leaving the
  asymmetry unguarded against an accidental future unification. (2)
  `resolve_file_ingest_context`'s `ResolveFileIngestError::IngestLookup`
  branch had no test. (3) `persist_pipeline_file_ingest`'s `None` arm
  (create-new-ingest path) of its `match existing` had no test — only
  the update-existing arm was covered. (4) The TOCTOU race where
  `update_ingest` returns `Ok(None)` (target deleted between lookup and
  update) had no test proving it surfaces as
  `PersistFileIngestError::IngestWrite`. (5) `remove_pipeline_file_ingest`
  had zero references anywhere in the test module — a completely
  untested function despite its siblings all being covered.
- Added regression coverage (5 new tests):
  `publish_auth_success_does_not_clear_prior_failure_state` pins the
  RTMP-vs-SRT `clear_on_success` asymmetry by recording an
  `RtmpPublish` failure, succeeding an auth, then showing a second
  failure still trips the ban (proving the first failure count
  survived the success).
  `resolve_file_ingest_context_surfaces_ingest_lookup_error` covers the
  `IngestLookup` error branch.
  `persist_pipeline_file_ingest_creates_new_ingest_when_none_exists`
  covers the create-path arm, asserting `create_ingest` is called with
  the `id_factory()`-generated ID.
  `persist_pipeline_file_ingest_surfaces_race_when_update_target_disappears`
  extends `FakeIngestWriter` with an `update_returns_none: bool` flag
  to simulate the TOCTOU race and asserts `IngestWrite` is returned.
  `remove_pipeline_file_ingest_deletes_all_ingests_and_clears_input_source`
  is the first-ever test for `remove_pipeline_file_ingest`, asserting
  both ingests tied to a stream key are deleted.
- Gates: `scripts/build/resource-limit.sh cargo test --lib ingest::` —
  62/62 pass (15 in `application::ingest`: 7 new plus 8 pre-existing;
  remainder pre-existing `application::srt_ingest`, `domain::srt_ingest`,
  `media::file_ingest` tests sharing the module-name filter).
  `scripts/build/resource-limit.sh cargo clippy --lib --benches -- -D
  warnings` — clean. `cargo fmt --all` — no changes beyond the new
  tests; `cargo fmt --all --check` — clean. `scripts/check/concurrency/fast.sh`
  — 135/135 pass (run per the `staged-gate-router`-recommended
  follow-up for this file).
- Commit: `1a2e66e0` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept
  `src/application/` files: `reconcile.rs` (648/12), `egress.rs`
  (549/7).

## 2026-07-19 03:55 HUNT RECONCILE-DECISION-BRANCH-COVERAGE DONE [codex]

- Scope: `src/application/reconcile.rs` (648 lines, 12 existing tests)
  — the pure decision functions (`decide_output_start_action`,
  `decide_output_stop_action`, `decide_recording_action`,
  `OutputRetryPolicy::backoff_ms`) plus the async
  `build_recording_reconcile_plan` orchestrator.
- Finding: no bug — six coverage gaps, all confirmed correct as
  designed. `decide_output_start_action`'s `NotApplicable` short-circuit
  (already-active or not desired-Running) had no test, nor did its
  plain `StartNow` path with no prior failure, nor the boundary where
  a `WaitRetry` window has fully elapsed and the action becomes
  `StartNow` again. `decide_output_stop_action`'s catch-all
  `KeepRunning` arm — the majority of its match's input space — had
  zero test coverage; only the two non-default arms were exercised.
  `OutputRetryPolicy::backoff_ms`'s `retries.min(16)` clamp was never
  proven to actually cap extreme retry counts. Most notably,
  `FakePipelineStore` already had an `error: Option<&'static str>`
  field wired into `list_pipelines`, but no test ever set it to
  `Some(_)` — the `PipelineStoreError` propagation path through
  `build_recording_reconcile_plan` was completely unexercised despite
  the fake being purpose-built for it.
- Added regression coverage (6 new tests):
  `start_action_is_not_applicable_when_already_active_or_not_desired_running`,
  `start_action_starts_now_without_prior_failure`, and
  `start_action_starts_now_once_backoff_window_elapses` cover the three
  previously-untested branches of `decide_output_start_action`.
  `stop_action_keeps_running_by_default` exercises all three inputs
  that fall through to the `KeepRunning` default arm.
  `backoff_ms_clamps_retries_beyond_shift_limit` asserts
  `backoff_ms(16) == backoff_ms(u32::MAX)`, pinning the clamp.
  `recording_reconcile_plan_propagates_pipeline_store_error` sets
  `FakePipelineStore.error` and asserts the error message survives
  through `build_recording_reconcile_plan`.
- Gates: `scripts/build/resource-limit.sh cargo test --lib reconcile::`
  — 18/18 pass (6 new plus 12 pre-existing). `scripts/build/resource-limit.sh
  cargo clippy --lib --benches -- -D warnings` — clean. `cargo fmt --all`
  — no changes beyond the new tests; `cargo fmt --all --check` — clean.
  `scripts/check/concurrency/fast.sh` — 135/135 pass (run per the
  `staged-gate-router`-recommended follow-up for this file).
- Commit: `ae7c42d3` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: continuing the open-ended scan. Remaining unswept
  `src/application/` file: `egress.rs` (549/7).

## 2026-07-19 04:10 HUNT EGRESS-MALFORMED-URL-RESILIENCE DONE [codex]

- Scope: `src/application/egress.rs` (549 lines, 7 existing tests) —
  the last unswept `src/application/` file by size. Unlike the prior
  three files in this run, this one is dense, sophisticated
  engine-integration coverage rather than thin glue: the existing 7
  tests already exercise ring reuse, HEVC->H.264 codec-edge sharing,
  audio-track-selection dedup, mixed-protocol codec pinning, codec-hint
  precedence over ingest meta, HLS terminal-stage reporting, and one
  full end-to-end packet-flow test through a real fixture. All of that
  held up under review with no gaps.
- Finding: no bug — one coverage gap. `prepare_output_ring` calls
  `OutputUrlScheme::from_url` and `EgressProtocol::from_url`
  (`src/domain/output_spec.rs`), both of which parse via
  `url::Url::parse(..).ok()` and fall back to `Unknown` for any
  unparseable string rather than panicking — but every existing test
  used a well-formed `rtmp://`/`srt://`/`https://` URL, so that
  fallback path was never exercised through `prepare_output_ring`
  itself. Confirmed the malformed-input path is handled safely as
  designed.
- Added regression coverage (1 new test):
  `prepare_output_ring_falls_back_gracefully_for_unrecognized_url_scheme`
  passes an output with url `"not-a-valid-url"` and asserts the
  function doesn't panic and produces the same source-passthrough
  result as a normal unencoded output (`Unknown` scheme is neither
  HLS-family nor RTMP, so no codec override or protocol segmenter
  applies).
- Gates: `scripts/build/resource-limit.sh cargo test --lib egress::` —
  9/9 pass (1 new plus 7 pre-existing `application::egress` tests, plus
  1 pre-existing `media::srt::srt_egress` test sharing the module-name
  filter). `scripts/build/resource-limit.sh cargo clippy --lib
  --benches -- -D warnings` — clean. `cargo fmt --all` — no changes
  beyond the new test; `cargo fmt --all --check` — clean. `egress.rs`
  is not on the AGENTS.md lifecycle-file list and the
  `staged-gate-router` did not recommend the concurrency fast-check for
  this commit.
- Commit: `b54baa74` on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — pure test-coverage addition, no production
  code changed.
- Notes: this closes out the largest-first scan of `src/application/`
  — all files in that directory have now been ratio-scanned and either
  hunted or ruled out this session. Next: pick the next-largest
  unswept directory (`src/media/` or `src/domain/`) for the following
  hunt.

## 2026-07-19 06:33 HUNT SECURITY-EVICTION-BAN-BYPASS FIXED [codex]

- Scope: `src/media/security.rs` — the ingest rate-limit/ban service
  (`IngestSecurityService`). Adversarial focus: the hard-cap eviction
  path in `evict_oldest_if_needed`, which runs whenever `state.len()`
  exceeds `tracked_ip_limit` after the normal expired-ban/idle-record
  retain pass.
- Finding: real bug, security-relevant. The hard-cap fallback sorted
  candidate entries by `r.failures.iter().copied().min()` (each
  record's *earliest* failure) and never consulted `banned_until` at
  all, then evicted the lowest-ranked `excess` entries outright. Two
  compounding defects: (1) ignoring ban status meant a currently-banned
  record was exactly as evictable as any other; (2) ranking by the
  earliest failure instead of the most recent one inverted the
  intended "evict the stalest/least-active entry" policy — a
  long-running attacker's first failure is always older than a
  fresh one-off flood IP's only failure, so the sort preferentially
  targeted active attackers over disposable ones. Combined, an
  attacker with an active ban could clear that ban well before
  `ban_ms` elapsed by flooding `record_failure` from enough disposable,
  unrelated IPs to push `tracked_ip_limit` and force their own banned
  record out. This directly contradicted the function's own inline
  comment, which claimed the logic existed specifically to prevent an
  attacker clearing their own record by flooding from many IPs.
  Confirmed empirically before fixing: three successive throwaway
  probe tests using `panic!` to print real eviction order under a
  crafted flood, narrowing from "sort order looks backwards" to a
  decisive repro where an active ban was evicted by an unrelated
  flood.
- Fix: replaced the sort key with a tuple `(currently_banned: bool,
  most_recent_failure: Instant)`. Tuple `Ord` gives banned status a
  hard priority tier — a banned entry is never evicted ahead of a
  non-banned one, regardless of individual recency — and within each
  tier, entries are ranked by their *most recent* failure (`max()`,
  not `min()`), so genuinely stale/inactive entries are evicted first
  within a tier. Updated the surrounding comment to explain both the
  ban-priority and min-vs-max reasoning so the next reader doesn't
  reintroduce either defect independently.
- Added two permanent regression tests:
  `flood_of_other_ips_does_not_evict_an_active_ban` (bans one IP, then
  floods 16 disposable IPs past `tracked_ip_limit`, asserts the ban
  survives) and
  `eviction_prefers_non_banned_entries_over_banned_ones_regardless_of_recency`
  (proves banned status is a hard tier, not just a recency tiebreaker,
  at a tighter `tracked_ip_limit`). Also closed two smaller coverage
  gaps found while reading the file: `rate_limit_scope_from_key_round_trips_and_rejects_unknown_keys`
  (no prior direct test of the `RateLimitScope::from_key` reverse
  mapping) and `embedded_null_byte_in_ip_does_not_forge_a_different_scope_key`
  (proves an attacker-controlled IP string containing the same NUL
  delimiter `scoped_key`/`parse_scoped_key` use internally can't
  masquerade as a different scope's ban record — `split_once` only
  ever splits at the first NUL, so it can't be exploited, but this was
  previously unverified).
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  media::security::` — 17/17 pass (4 new plus 13 pre-existing,
  including the pre-existing `tracked_ip_limit_is_enforced` and
  `concurrent_is_ip_banned_no_deadlock_or_wrong_result`, confirming the
  cap is still enforced and concurrent access is still race-free).
  `cargo fmt --all` / `--check` — clean. `scripts/build/resource-limit.sh
  cargo clippy --lib --benches -- -D warnings` — clean (clippy first
  caught an ambiguous `\0198...` octal-looking escape in the embedded-
  NUL test literal; fixed with an explicit `\x01` hex escape).
  `security.rs` is not on the AGENTS.md lifecycle-file list and the
  fix only changes the sort key evaluated inside an already-locked
  critical section — no new lock, no new thread-hop, no change to what
  the `RwLock` protects — so the heavier `fast.sh`/`contract.sh`
  concurrency gates were judged unnecessary; the existing
  `concurrent_is_ip_banned_no_deadlock_or_wrong_result` test already
  covers concurrent-access correctness and still passes unchanged.
- Commit: (this commit) on `codex/adversarial-hunt-round2-20260718`.
- Follow-ups: none filed — the fix is scoped and fully covered by the
  two new regression tests.
- Notes: this is the first genuine bug (as opposed to a routine
  coverage-gap addition) found so far in this hunt run, hence the
  journal entry — per this session's practice of journaling real
  findings and skipping journal entries for routine test-coverage
  additions.

## 2026-07-19 HUNT STAGE-LIFECYCLE-STALE-SPAWN-METADATA FIXED [codex]

- Scope: `src/media/stage_lifecycle.rs` — `StageLifecycle::transition`,
  the single mutation point for a stage's tracked phase, backend kind,
  and backend pid. Zero prior journal mentions; picked as the next
  target after three consecutive routine-coverage hunts
  (`h264_transcoder.rs`, `tcp_stats.rs`, `file_analysis.rs`) turned up
  no bugs.
- Finding: real bug, state-corruption class. `transition()` had an
  existing guard that suppresses the *phase* field when a stale
  `StartingBackend`/`BackendSpawned` event arrives after
  `first_input_at` is already set and the stage has moved on to
  `FirstInput`/`FirstOutput`/`Producing` — intended to stop a
  late/duplicate spawn notification from regressing operator-visible
  progress. But the function updated `inner.backend` and
  `inner.backend_pid` *before* evaluating that guard, unconditionally,
  on every call. So a suppressed transition still silently overwrote
  the backend kind and pid while leaving the phase untouched,
  producing an inconsistent snapshot: phase says "still running the
  original backend" while `backend`/`backend_pid` reflect a spawn
  attempt that was never actually adopted into the visible phase.
  Traced a plausible real trigger: `get_or_create_stage_lifecycle_with_backend`
  (`stage_registry_access.rs`) reuses the same `StageLifecycle` Arc
  across a stage's restarts, and `run_external_ffmpeg_backend`
  (`external_transcoder.rs`) always calls `transition(BackendSpawned
  {..})` after a successful subprocess spawn — if a stale/duplicate
  spawn task's notification races in after a newer instance has
  already reached `FirstInput`, the metadata corruption is directly
  observable in operator diagnostics via `snapshot()`.
- Fix: reordered `transition()` to evaluate the stale-event guard
  first and `return` before touching `inner.backend` /
  `inner.backend_pid`, so a suppressed transition is now fully
  suppressed — phase, backend, and pid all stay put together, or all
  update together. No change to the guard's condition itself, only to
  what it protects.
- Added one permanent regression test:
  `stale_backend_spawned_transition_does_not_corrupt_backend_metadata`
  — constructs a lifecycle already `BackendSpawned{ExternalFfmpeg,
  pid: 111}`, records first input, then feeds a second
  `BackendSpawned{InternalFfmpeg, pid: 222}` transition and asserts
  both `backend` and `backend_pid` still read the original values
  alongside the still-suppressed `FirstInput` phase. This test fails
  against the pre-fix code and passes after.
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  stage_lifecycle::` — 14/14 pass (1 new plus 13 pre-existing,
  including the existing `backend_spawned_transition_does_not_regress_after_first_input`
  and `backend_pid_survives_runtime_phase_progression`, confirming no
  regression to the intended phase-suppression or pid-persistence
  behavior). `cargo fmt --all --check` — clean. `scripts/build/resource-limit.sh
  cargo clippy --lib --tests -- -D warnings` — clean.
  `scripts/check/concurrency/fast.sh` — 135/135 pass (this file backs
  lifecycle state read by `engine.rs`/`stage_runtime.rs`, so the
  broader concurrency gate was run even though the fix itself only
  reorders statements inside an already-locked critical section — no
  new lock, no new thread-hop). `scripts/check/source-audit.sh` —
  passed.
- Commit: (this commit) on `codex/adversarial-hunt-round3-20260719`.
- Follow-ups: none filed — the fix is scoped and fully covered by the
  new regression test; the sibling file `startup_policy.rs` (409
  lines, also zero prior journal mentions) is the next unswept
  candidate for this hunt run.
- Notes: second genuine bug found this hunt run (after the
  `security.rs` eviction-bypass finding on round 2), hence the journal
  entry.

## 2026-07-19 HUNT TRANSCODER-SCALE-PATH-PTS-DEFAULT FIXED [codex]
- What: hunt target `src/media/transcoder.rs` (1840 lines pre-fix,
  largest genuinely-unswept file in `src/media/` after `engine.rs`;
  re-verified zero prior journal mentions via a looser filename grep
  after the exact-backtick-match ranking method turned out unreliable
  — it had falsely shown 0 mentions for `stage_lifecycle.rs`, which
  had literally just been hunted in this same run).
- Finding: real bug, timestamp-corruption class, same family as the
  already-fixed and already-tested M7 issue in this file. The
  passthrough demux path (`run_ffmpeg_transcoder_stage_with_normalizer`)
  correctly skips encoder/demux packets with `AV_NOPTS_VALUE`
  (`pts() == None`) rather than defaulting to 0, because on a
  long-running stream a 0 substitution produces a massive backward
  timestamp jump (e.g. -3,600,000ms after 1 hour) that corrupts
  `DtsEnforcer` downstream — this is exactly what the existing
  `pts_zero_would_produce_zero_ms_timestamp` test documents. But the
  sibling decode-scale-encode path
  (`run_ffmpeg_transcode_with_scale_with_normalizer`) was never
  hardened the same way: both of its encoder-output receive loops
  (the steady-state loop and the EOF flush loop) used
  `enc_pkt.pts().unwrap_or(0)`, silently emitting a `pts=0` packet
  instead of skipping whenever the encoder produced a packet without
  a pts — an inconsistency between two code paths in the same file
  where only one had been hardened against the same failure mode.
- Fix: replaced both `unwrap_or(0)` fallbacks with
  `let Some(pts_ms) = enc_pkt.pts() else { continue };`, matching the
  passthrough path's skip behavior exactly. `dts_ms` still falls back
  to `pts_ms` via `enc_pkt.dts().unwrap_or(pts_ms)`, unchanged — that
  fallback is fine because it only runs once `pts_ms` is already known
  valid.
- Added one permanent regression test:
  `scale_encode_path_skips_none_pts_like_passthrough_path` in
  `src/media/transcoder.rs`'s `#[cfg(test)]` module. A live FFmpeg
  pipeline can't be made to emit `AV_NOPTS_VALUE` from a unit test (no
  checked-in fixture naturally produces a decoder frame lacking a
  pts), so — following the same precedent as
  `pts_zero_would_produce_zero_ms_timestamp` in this exact file — the
  test asserts the fix's *shape* directly: it scans this file's own
  source for the skip pattern (needle built from non-adjacent string
  fragments so the test's own source can't self-match) and asserts it
  appears exactly twice (once per encoder-output loop), and separately
  asserts the zero-default fallback pattern is absent. This fails
  against the pre-fix code (0 skip occurrences, fallback present) and
  passes after.
- Gates: `scripts/build/resource-limit.sh cargo test --lib
  media::transcoder` — 25/25 pass (1 new plus 24 pre-existing, no
  regressions to the existing pts/audio-routing/audio-router coverage).
  `scripts/build/resource-limit.sh cargo test --test transcoder` —
  14/14 integration tests pass unchanged, including the real-pipeline
  `internal_transcode_builtin_video_presets_produce_video` and
  `internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order`
  tests that exercise the fixed function against real FFmpeg encodes.
  `cargo fmt --all --check` — clean. `scripts/build/resource-limit.sh
  cargo clippy --lib --tests -- -D warnings` — clean.
  `scripts/check/source-audit.sh` — passed. No benchmark re-run: the
  fix only changes which rare edge-case packets are skipped vs.
  defaulted (a `None`-pts branch that real fixtures never hit), and
  does not change the per-packet allocation, locking, or channel-send
  pattern on the hot path.
- Commit: (this commit) on `codex/adversarial-hunt-round3-20260719`.
- Follow-ups: none filed — fix is scoped and covered by the new
  regression test. `codec.rs` (1594 lines) and `feeder.rs` (913 lines)
  remain as the other confirmed genuinely-unswept `src/media/` files
  from this hunt run's re-ranking.
- Notes: third genuine bug found this hunt run (after the
  `security.rs` eviction-bypass finding on round 2 and the
  `stage_lifecycle.rs` stale-spawn-metadata finding earlier this run).
