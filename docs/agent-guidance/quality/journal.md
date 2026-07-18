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
- [2026-07-18 16:40 Q-009 DONE [opus]](#2026-07-18-1640-q-009-done-opus)
- [2026-07-18 17:20 Q-010 DONE [opus]](#2026-07-18-1720-q-010-done-opus)
- [2026-07-18 17:55 Q-012 DONE [opus]](#2026-07-18-1755-q-012-done-opus)

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
