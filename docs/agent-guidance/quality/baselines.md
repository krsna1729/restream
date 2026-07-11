# Performance & Resource Baselines

Durable measurement ledger for perf-sweep. Criterion's `target/criterion/`
state is scratch; this file is the source of truth for "did we regress".

Rules: measurements are serial (idle host, kill-check first), bench profile
only, recorded with date + commit. Update a row only with fresh numbers from
this machine; never copy numbers you did not measure. Historical sections are
reference points — do not overwrite them, add new dated rows.

## Benchmark ledger (Criterion medians)

| Suite | Metric | Median | Noise ± | Commit | Date | Last verified |
|---|---|---|---|---|---|---|
| ring_buffer | (seed via Q-003) | — | — | — | — | — |
| avio_throughput | (seed via Q-003) | — | — | — | — | — |
| high_performance_data_path | (seed via Q-003) | — | — | — | — | — |
| matrix_throughput | — | — | — | — | — | — |
| srt_ingest_latency | — | — | — | — | — | — |
| transcoder_throughput | — | — | — | — | — | — |
| hls_cost | — | — | — | — | — | — |
| hls_fmp4_cost | — | — | — | — | — | — |
| stage_feeder | — | — | — | — | — | — |
| stage_metrics | — | — | — | — | — | — |
| codec_conversions | — | — | — | — | — | — |
| simd_alternatives | — | — | — | — | — | — |
| alert_tracker | — | — | — | — | — | — |

Default regression threshold: ±5% on throughput suites unless a row notes
otherwise. A regression beyond threshold is filed, not silently absorbed.

## Resource ledger (resource-sweep / scale runs)

| Config | RSS | Ring payload | AVIO peak HWM | Blocked writes | Commit | Date |
|---|---|---|---|---|---|---|
| (seed via Q-006) | — | — | — | — | — | — |

### Historical reference — 2026-06-27 memory-optimization pass

After ring/AVIO/TS sizing cuts (−205 MB RSS total across 15 scale cases,
−175 MB ring payload, zero ring overflows):

| Config | RSS after | Ring payload after |
|---|---|---|
| h265-srt 4M | 205 MB | 47 MB |
| h265-srt-multi 8M | 237 MB | 77 MB |
| h264-srt-multi 8M | 137 MB | 71 MB |
| h264-rtmp 8M | 116 MB | 35 MB |

### Internal video-preset rollout RSS baseline — 2026-07-10

Command:

```sh
RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
ONLY_CHECKS=load,ffprobe,decode-scan \
scripts/harness/rollouts/internal-video-presets.sh
```

RSS guard baseline: `test/harness/baselines/internal-video-presets-rss.csv`.
Regression threshold: `20%` per-output RSS for this rollout guard, allowing
host/process jitter while catching large memory regressions.

| Scenario | Outputs | Restream RSS delta | Per-output RSS | External FFmpeg RSS | Commit |
|---|---:|---:|---:|---:|---|
| `mixed.live.srt.h264.a1.bf0` | 12 | 113,388 KB | 9,449 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a1.bf2` | 12 | 134,004 KB | 11,167 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a2.bf0` | 30 | 128,576 KB | 4,285 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a2.bf2` | 30 | 150,024 KB | 5,000 KB | 0 KB | this commit |

Jitter headroom by design (defaults; env-overridable):

| Ring | Default slots | Typical rate | Headroom |
|---|---|---|---|
| Source (SRT ingest) | 1024 | 80 pkt/s | 12.8 s |
| Source (2v16a adaptive) | 4980 | 830 pkt/s | 6.0 s |
| Transcoder output | 512 | 80 pkt/s | 6.4 s |
| TS mux ring | 256 | ~400 chunks/s | 0.64 s (SRT 12 MB send buffer absorbs the rest) |
| AVIO queue | 512 KB | 1 MB/s @ 8 Mbps | 0.5 s |

### External capacity rollout proof — 2026-07-10

Command:

```sh
scripts/harness/rollouts/external-capacity.sh
```

The guard runs `mixed.live.srt.h264.a2.bf0` twice: first with enough external
FFmpeg permits for a passing capacity smoke, then with one permit and default
checks enabled. The constrained leg must fail causally with
`blockedByPhase=waitingForCapacity`, `backend=externalFfmpeg`, nonzero `waitMs`,
and at least one persisted `ready` recording row with both `temp_path` and
`final_path`.

| Scenario | Capacity-ok permits | Constrained permits | Required constrained evidence | Commit |
|---|---:|---:|---|---|
| `mixed.live.srt.h264.a2.bf0` | 2 | 1 | `waitingForCapacity`, `externalFfmpeg`, `waitMs>0`, ready recording metadata row | this commit |

## Standing optimization targets (2026-06-27 CPU profile, task-clock 999 Hz)

| Self % | Symbol | Meaning | Backlog |
|---|---|---|---|
| 3.28% | `__memmove_avx_unaligned_erms` | AVIO buffer → `ts_accum` copy | Q-009 [opus] |
| 2.60% | `pthread_mutex_lock` | SRT internal + MemoryQueue mutex | (unfiled) |
| 1.18% | `__vdso_clock_gettime` | per-packet SRT latency tracking | (unfiled) |
| 0.87% | `_int_malloc` | per-packet `Arc::new(MediaPacket)` | Q-010 [opus] |
| 0.43% | `VecDeque::extend` | AVIO queue write (second copy) | Q-009 [opus] |

## Profiling notes (WSL2)

Hardware PMU counters are unavailable under Hyper-V. Use
`perf record -e task-clock` (software sampling), `perf_event_paranoid=-1`,
with the distro `linux-tools-generic` perf binary.
