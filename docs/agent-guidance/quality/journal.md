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
