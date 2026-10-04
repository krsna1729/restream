# WI11 Rejected Experiments

Three WI11 follow-up branches were measured and not merged: CPU segregation
(`wi11/cores`), HLS PUT on cyper (`wi11/hls-cyper`) and an alternative global
allocator (`wip/alloc-ab`). This page records what each branch changed, how it
was measured, what the numbers show, why the result came out that way, and what
stays open. The roadmap entries (§36 items 4, 8 and 15 in
[srt-compio-roadmap.md](srt-compio-roadmap.md)) summarise and point here.

## Contents

- [Method](#method)
- [CPU segregation (wi11/cores)](#cpu-segregation-wi11cores)
- [HLS PUT on cyper (wi11/hls-cyper)](#hls-put-on-cyper-wi11hls-cyper)
- [Global allocator (wip/alloc-ab)](#global-allocator-wipalloc-ab)
- [Superseded branches](#superseded-branches)
- [Reproducing](#reproducing)

## Method

All runs use release binaries and `cargo xtask capacity-ramp` on the
6-vCPU KVM host (AMD EPYC, 11.9 GB). Restream and the harness (publisher and
receiver sinks) run on disjoint CPU sets. The arms of each comparison run
interleaved (A, B, A, B), so slow drift on the host affects both arms alike.
CPU is Restream's average over the rated window, in percent of one core. A
rung passes when every destination received at least 0.95 of the offered rate
(HLS: every due segment within 3 s).

The host rebooted from kernel 7.0.0-34 to 7.0.0-38 between the CPU-segregation
runs and the cyper and allocator runs. After that reboot, SRT×100 fails
intermittently because the harness SRT sink drops datagrams in its socket
receive buffer (backlog Q-027). Restream's own send rate is healthy in those
runs. SRT results from either kernel must be read with that in mind; RTMP and
HLS results are not affected.

## CPU segregation (wi11/cores)

**What the branch did.** It added three CPU sets: `RESTREAM_CONTROL_CPUS`,
`RESTREAM_HOT_CPUS` and `RESTREAM_MEDIA_CPUS`. Each thread pinned itself at
start: Tokio workers, the blocking pool and the main thread to the control set;
egress shards, ingress owners and FFmpeg I/O threads to the hot set; the media
executor and FFmpeg decode/encode (in-process threads and child processes) to
the media set. Thread counts followed the sets: Tokio workers equal to the
control set's size, the egress shard ceiling from the hot set's size, media
workers from the media set's size. Sets outside the process's starting mask
disabled the feature. With no sets configured it did nothing.

**Question.** Does keeping control, hot-path and media threads on separate
CPUs reduce cost or improve delivery, compared with letting the kernel schedule
them freely?

**Setup.** Kernel 7.0.0-34. Restream on CPUs 0–3, harness on 4–5. Segregated
arm: control = CPU 0, hot = CPUs 1–2, media = CPU 3. Two repeats per rung, 20 s
windows.

**Results against the default.**

| Rung | Default CPU (passes) | Segregated CPU (passes) |
|---|---|---|
| RTMP×1000 | 147%, 134% (4/4) | 112%, 111%, 111%, 105% (8/8) |
| HLS×500 | 69%, 69% (4/4) | 57%, 55% (4/4) |
| SRT×100 | 138%, 151%, 125%, 138% (8/8) | 128%, 118%, 110%, 107%, 114%, 115% (9/12) |
| SRT×50 | 113%, 89% (4/4) | 89%, 84% (4/4) |

Segregation used 20–25% less CPU on RTMP and HLS, and peak RSS at RTMP×1000
fell from 263–264 MB to 203–207 MB.

**But the arms differ in more than placement.** The default arm sizes from
4 CPUs: up to 4 egress shards, 2 Tokio workers, 4 media workers. The segregated
arm sizes from its sets: 2 shards (2 hot CPUs), 1 Tokio worker, 1 media worker.
So a control arm was added: the same thread counts as the segregated arm (2
shards, 1 Tokio worker) with no pinning.

| Rung | Matched counts, no pinning | Segregated (same counts) |
|---|---|---|
| RTMP×1000 | 104% (1/2), 102% (2/2) | 111% (2/2), 105% (2/2) |
| SRT×100 | 128% (1/2), 129% (1/2) | 114% (1/2), 115% (2/2) |

With thread counts equal, pinning did not save CPU on RTMP (it cost 3–6%). On
SRT it saved about 11% (114% against 128%) and passed 3 of 4 repeats against
2 of 4. The matched arm also failed one RTMP×1000 repeat (minimum ratio 0.948,
just under the floor).

**Why.** The saving came from running fewer threads, not from where they ran.
Fewer shards and workers mean fewer wakeups, fewer context switches and less
cross-thread traffic on the same 4 CPUs; that is the "minimise sharing" effect
seen elsewhere (one shard per feed, [runtime-crossings.md](runtime-crossings.md)
M6). Pinning by itself adds nothing on RTMP, where the hot threads already
stay busy and the kernel keeps them in place.

**The SRT delivery numbers need care.** Each SRT repeat also records the
harness sink's UDP receive-buffer drops:

- The segregated arm's failing repeats came with heavy receiver drops (means
  of 4.0–5.5k/s, peaks of 23–39k/s). Passing repeats in every arm peaked at
  ≤ 10k/s. Those failures are the receiver-side loss later isolated in Q-027,
  so they do not show that segregation hurts delivery. An earlier summary said
  it did; that is withdrawn.
- The matched arm's two failures (69 and 66 of 100 delivered) had low
  receiver drops (means of 155–339/s). Those are on Restream's side: with
  2 shards and 1 Tokio worker unpinned, SRT×100 was marginal on 4 CPUs.

So the default (more threads) delivered SRT×100 reliably; cutting threads made
it marginal; pinning the reduced set recovered some of that. With 2–4 repeats
per cell, none of these differences is firm.

**Decision.** Not merged. The CPU saving against the default comes from
running fewer threads, which needs no affinity code. Pinning itself showed no
gain on RTMP and a small, unconfirmed one on SRT. Affinity code was already
rejected in Q-012 ([baselines.md](agent-guidance/quality/baselines.md)) in
favour of a process-level cpuset, which the kernel enforces on every thread for
the whole process lifetime; nothing here overturns that.

**What stays open.**
- The thread-count signal: RTMP×1000 on 2 shards cost about 25% less CPU than
  on 4, and passed 11 of 12 repeats (one at 0.948). That is near the delivery
  edge, so it argues for measuring the RTMP cold prior of 128 outputs per
  shard (Q-025), not for changing it now.
- The ~11% SRT difference with matched counts is worth one more A/B after the
  harness sink stops dropping (Q-027).

## HLS PUT on cyper (wi11/hls-cyper)

**What the branch did.** It split the HLS PUT uploader into a transport trait
with two implementations. The default kept Reqwest on Tokio. The
`hls-put-cyper` feature moved every uploader to one dedicated
`restream-hls-put` thread running a Compio runtime and one shared cyper client
(cyper is hyper running on Compio). The upload loop itself was shared and
unchanged: **one task per output, waking every 500 ms**, snapshotting the
segment list and checking it against that output's own uploaded set.

**Question.** Does moving HLS PUT off Tokio onto Compio reduce cost or
improve upload latency?

**Setup.** Kernel 7.0.0-38. Restream on CPUs 0–2, harness on 3–5. Two rounds of
two repeats per rung, 30 s windows. Both binaries built from the same commit
(`a36fbdb5` and later `0b7323bc`), differing only in the feature flag.

**Results.**

| Rung | Measure | Reqwest | cyper |
|---|---|---|---|
| HLS×500 | CPU | 59–63% | 52–72% |
| HLS×500 | p99 segment lag | 0.53–0.71 s | 1.1–1.8 s |
| HLS×500 | peak RSS | 0.22 GB | 3.8–4.5 GB |
| HLS×1000 | CPU | 95–97% | 89–105% |
| HLS×1000 | p99 segment lag | 0.94–1.21 s | 2.4–5.4 s |
| HLS×1000 | passes | 4/4 | 2/4 |
| HLS×1000 | peak RSS | 0.36 GB | 8.8–9.2 GB |

Total CPU did not change. The work moved: with Reqwest, Tokio spent 87–89% of
a core at HLS×1000; with cyper, the new thread spent 53–68% and Tokio 27–29%.

**Why the memory.** A focused probe (HLS×500, same commit, both builds)
sampled Restream's TCP connections and anonymous memory every 2 s:

| | Reqwest | cyper |
|---|---:|---:|
| Established TCP connections | 503 (about 1 per output, reused) | 795 (more than outputs) |
| Peak anonymous memory | 161 MiB | 4,455 MiB |
| Per output | 329 KiB | 9.1 MiB |
| Growth pattern | flat once connected | steps with the connection count: 386 connections → 1.7 GB, 795 → 4.4 GB |

The memory is held per connection, about 5 MB each, and Restream's own
buffers (source ring, HLS store) are the same in both builds. The cause is in
how cyper connects hyper to Compio. hyper writes through a poll-based
interface; Compio submits owned buffers to io_uring. cyper bridges the two
with `compio-io`'s `SyncStream` adapter, which copies every written byte into a
per-connection write buffer that may grow to 64 MiB and shrinks only after it
fully drains (`compio-io` 0.10.1, `src/compat/sync_stream.rs`). Each segment
PUT (about 1 MB at 8 Mbit/s) is copied whole into its connection's buffer, the
read side keeps its own buffer, and cyper opened more connections than there
were outputs. That is an extra copy of every uploaded byte plus segment-sized
buffers held per connection.

**Why the latency.** All uploads ran on one thread. Every byte took the extra
copy on that thread, and each output still polled on its own 500 ms timer. A
single slow write delays every other upload behind it on the same thread
[INFERENCE: from the thread layout and the lag growth; not traced per
request].

**Decision.** Not merged. It fails on latency, reliability and memory, and
saves no CPU.

**What this does not show.** It does not show that HLS PUT should stay on
Tokio. The experiment changed only the transport and kept one polling task per
output. The structural problem is that per-output shape: per-output timers,
per-output snapshots, a per-output Reqwest client (each uploader builds its own
`reqwest::Client`, so connection pools and TLS state are per output too). HLS
PUT is HTTP over TCP, the same transport class as RTMP egress, and fits the
egress-shard model: leaves on a shard thread, segment-publish wakes instead of
polling, owned `Bytes` submitted to io_uring without a copy, kTLS for HTTPS as
RTMPS does. That is the planned follow-up; it has to be measured against the
Reqwest numbers above.

## Global allocator (wip/alloc-ab)

**What the branch did.** It added two build features, `alloc-mimalloc` and
`alloc-jemalloc`, each installing a `#[global_allocator]`. The default build
stays on glibc malloc (with Restream's provisional `MALLOC_ARENA_MAX` of 2).

**Question.** An earlier SRT-ingest profile showed mimalloc reducing ingress
Owner media time by 40% and total CPU by 13% at 32 publishers, for 90 MB more
RSS. Does that hold for fan-out egress?

**Setup.** Kernel 7.0.0-38. Restream on CPUs 0–3, harness on 4–5. Three
binaries built from `9c704798`, differing only in the allocator. Two rounds of
two repeats, 20 s windows.

**Results.**

| Rung | glibc | mimalloc | jemalloc |
|---|---|---|---|
| RTMP×1000 CPU | 118%, 122% | 116%, 106% | 119%, 117% |
| RTMP×1000 peak RSS | 263–266 MB | 270–287 MB | 306–313 MB |
| HLS×500 CPU | 63%, 57% | 56%, 58% | 58%, 55% |
| HLS×500 peak RSS | 210–226 MB | 190–195 MB | 207–214 MB |

Every RTMP and HLS repeat passed. SRT×100 failed in every arm, glibc included,
because of the receiver-side drops of Q-027, so SRT could not be judged.

**Reading.** The spread between two rounds of the same binary (glibc RTMP
118% vs 122%; mimalloc 116% vs 106%) is as large as any difference between
allocators. mimalloc's best RTMP round is the only value outside glibc's range,
and its other round is not. jemalloc costs 16–18% more RSS on RTMP for no CPU
gain. mimalloc used about 10% less RSS on HLS and 3–8% more on RTMP.

**Decision.** Keep glibc. No fan-out win is established, and adding a C
allocator adds build and supply-chain surface.

**What stays open.** The SRT-ingest result (mimalloc −13% total CPU at 32
publishers) was never repeated. It is a different workload: many small
payloads allocated on the ingress Owner and freed by readers on other threads,
which is where glibc arena contention shows. Repeat it once the harness sink is
fixed (Q-027), with both ingest and fan-out rungs.

## Superseded branches

- `wi11/ingress-shards`: RTMP ingress across `SO_REUSEPORT` owners. Landed in
  a reworked form as #201 (`RESTREAM_RTMP_INGRESS_OWNERS`, default 1).
- `codex/rx-owner-tsmux`: shared TS mux and HLS segmenters on a dedicated
  ring-woken worker pool. The same WI11 step landed as the `restream-media`
  pool (#197, #199). Its own A/B showed no CPU gain (121.4% vs 120.7%).

## Reproducing

The branches are deleted. Their final patches, including uncommitted state,
are archived on the measurement host under `.local/artifacts/`:
`wi11-cores-final.patch`, `wi11-hls-cyper-final.patch`, `alloc-ab-final.patch`
(and the superseded `wi11-ingress-shards-superseded.patch`,
`rx-owner-tsmux-superseded.patch`). Run artifacts are under
`.local/artifacts/from-wi11-cores/`, `from-wi11-hls-cyper/`, `from-alloc-ab/`
and `cyper-probe/`. To rerun one arm, apply a patch to its base commit, build
with `cargo xtask build-release` (add the feature flag for cyper or an
allocator), and run `cargo xtask capacity-ramp` with the CPU split and
ladder given in each setup above.
