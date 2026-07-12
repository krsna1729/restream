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
- Notes: current live sample used 4.276 Restream CPUs with IPC 0.37,
  19.93% cache misses, 10.21% branch misses, and 130.755 migrations/sec.
  Thread census again showed six hot Tokio scheduler workers plus roughly one
  low-CPU `SRT:RcvQ:*` thread per SRT socket.
