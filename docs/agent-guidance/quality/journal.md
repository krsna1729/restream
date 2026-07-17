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
