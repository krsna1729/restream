# Quality Loop Journal

Append-only log of quality-loop iterations. Newest entries at the bottom.
Entry format: `docs/agent-guidance/skills/quality-loop/SKILL.md` § Journal entry format.
Do not edit or delete past entries; corrections get a new entry.

Grooms archive resolved history from `backlog.md` into this file's commit
trail — the journal plus `git log --grep "quality("` is the full audit record.

---

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
  context switches `7.507 K/sec`, and migrations `909.133/sec` while MediaMTX
  remained `1200/1200` with bytes growing before and after the perf window.
