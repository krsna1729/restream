# Egress Capacity Ramp

How to measure Restream's RTMP, RTMPS and SRT fan-out capacity on any Linux
host, reproducibly, and compare hosts. One 8 Mbit/s H.264 ingest fans out to
N outputs of one protocol; the receiver is the harness's own in-process sink
(`MSR_PEER=sink`), because mediamtx saturates first (it received almost
nothing at 100 SRT outputs). Restream and the receiver apparatus run on
disjoint CPU sets.

## Contents

- [What it measures](#what-it-measures)
- [Run it](#run-it)
- [Reading the results](#reading-the-results)
- [Reference results](#reference-results)
- [Prompt for running on another machine](#prompt-for-running-on-another-machine)

## What it measures

Per rung (protocol × output count × repeat), from one `test_harness
resource-sweep` run:

- **Receiver delivery**: per-destination bytes at the sink over the rated
  window divided by the offered (ingest) rate. A destination is delivered at
  ≥ 0.95. A rung **passes** when every destination was delivered.
- **HLS PUT** is graded per segment, not by bytes: a 30 s window holds only
  about ten segments, so a byte ratio is too coarse. A segment is due once any
  output has it and 3 s have passed; an output is delivered when it received
  every due segment of the window, each within 3 s of the first output
  (`resource-sweep-results.json` `delivery.hls` carries the lag p50/p99/max).
- **Worst interval ratio**: the lowest one-sample ratio, which exposes stalls
  an average hides (not applicable to HLS PUT).
- **Jain fairness** over destination rates.
- **Restream's own view** (`/api/v1/pipelines/{id}/telemetry` `delivery`):
  RTMP from TCP `bytes_acked`, SRT from ACK coverage (an upper bound; see
  [observability](observability.md#per-destination-delivery)).
- Restream CPU (average and peak, % of one core), RSS and thread count.

**Capacity** for a protocol is the highest rung where every repeat passed.
Default ladders stop at 1000 outputs per protocol (`CAPACITY_*_OUTPUTS`
overrides them); protocols default to `rtmp,rtmps,srt,hls`.

**Observation cost.** The harness polls `/api/v1/engine/health` and each
pipeline's telemetry every second, and Restream builds those responses as
`serde_json::Value` trees. At RTMP×100 (release, 43b68dc0) API handling was
~21% of Restream samples, the health snapshot alone 7–10%, and it grows with
output count; Restream CPU in the tables includes it.

## Run it

Prerequisites: a Linux host with `io_uring` enabled (kernel ≥ 6.1), the `tls`
kernel module for RTMPS kTLS (`sudo modprobe tls`), ≥ 4 online CPUs, ≥ 8 GB
RAM, and the repository toolchain (`scripts/dev/bootstrap.sh` on Debian/Ubuntu,
then `scripts/dev/prepare.sh`). Nothing else may run media on the host.

Recommended sysctls (the script warns when lower):

```sh
sudo sysctl -w net.core.somaxconn=4096 \
  net.core.rmem_max=26214400 net.core.wmem_max=26214400
```

Run on a clean checkout of the agreed commit:

```sh
git checkout <agreed commit>
scripts/harness/capacity-ramp.sh
```

It builds release binaries (`scripts/build/release-harness.sh`, into
`target/qual-release/`; `CAPACITY_BUILD_PROFILE=bench` uses the inner-loop
bench profile instead), refuses a
dirty worktree or running `restream`/`mediamtx`/`ffmpeg`, and writes
`.local/artifacts/capacity-ramp/<utc stamp>/` with `summary.md`,
`summary.csv`, `provenance.json` and one resource-sweep directory per rung
and repeat. `scripts/harness/capacity-ramp.sh --help` lists every knob.

CPU split. By default Restream gets the first half of the online CPUs and the
harness, publisher and sinks the second half. On large hosts:

- Split by NUMA node when there is more than one: `CAPACITY_RESTREAM_CPUS` =
  one node's CPUs, `CAPACITY_HARNESS_CPUS` = another's (`lscpu -e` shows the
  mapping). Record which you chose.
- The product default egress shard count is CPU-derived and clamped to
  `2..=8`. Run once with the default, then again with
  `CAPACITY_EGRESS_SHARDS=<number of Restream CPUs>` to see whether more
  shards buy capacity.
- `CAPACITY_SINK_THREADS` defaults to the harness CPU count; raise
  `CAPACITY_PEER_COUNT` (more sink ports/listeners) if the receiver, not
  Restream, saturates. The sink port count also changes Restream's SRT cost:
  srt-rs GSO coalesces consecutive datagrams to one destination address, so
  outputs sharing a sink port share sends. SRT×50, same binary, interleaved:
  one port 102–105% CPU with ~10 datagrams per batched send; three ports
  135–144% with ~2.9; one port per output (the real-fan-out case) 142–156%
  with ~1. The default (one port per sink thread) is therefore close to, but
  slightly under, the cost of fully distinct destinations; compare SRT rows
  only across runs with the same `peer_count`. Check the sink side with `top` during a high rung: the
  `test_harness` process should stay below its CPU budget.
- `CAPACITY_MALLOC_ARENA_MAX` sets Restream's glibc arena cap for the run
  (`default` = glibc policy). Restream's own default of 2 was measured on one
  6-CPU host only; qualify it per host before treating it as final.
- Ladders stop at 1000 outputs by default; go beyond only when that is the
  question being asked (`CAPACITY_*_OUTPUTS`). A ladder stops after a rung
  with no passing repeat (`CAPACITY_STOP_AFTER_FAIL=0` to keep going).

A full default run takes roughly 1–2 hours (3 repeats, 30 s windows).

## Reading the results

- Compare capacity per protocol, and CPU per output at the same rung.
  Restream's CPU is reported in % of one core, so divide by the Restream CPU
  count for utilization.
- A rung that fails while Restream's CPU is well below its budget points at
  the receiver or the network stack; check the harness CPU and kernel drops
  (`nstat -az | grep -i -E 'drop|overflow|RcvbufErrors'`).
- SRT failure shape matters as much as the capacity number: note whether
  delivery degrades for a few destinations (graceful) or collapses for all.

## Reference results

Reference: 6-CPU AMD EPYC KVM VPS (1 NUMA node), kernel 6.8, commit
`1a2e7e12`, release binaries, `scripts/harness/capacity-ramp.sh` defaults
(Restream on CPUs 0–2, harness on 3–5, product-default shards, SRT sink
threads and peer count 3, 30 s windows, 3 repeats per rung; ladders capped at
1000). CPU is % of one core; Restream's budget is 300%. Artifacts:
`.local/artifacts/capacity-ramp/20260927T055241Z/`.

| protocol | capacity | rung | passed | rx delivered min | rx ratio min | CPU avg | CPU peak | RSS MB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| RTMP | **≥ 1000** (cap) | 100 | 3/3 | 100 | 0.971 | 32.6% | 64.9% | 110 |
| | | 250 | 3/3 | 250 | 0.959 | 82.0% | 130.5% | 133 |
| | | 500 | 3/3 | 500 | 0.964 | 99.5% | 172.5% | 178 |
| | | 1000 | 3/3 | 1000 | 0.964 | 155.8% | 210.2% | 253 |
| RTMPS | **≥ 1000** (cap) | 100 | 3/3 | 100 | 0.970 | 38.7% | 64.4% | 105 |
| | | 250 | 3/3 | 250 | 0.963 | 111.2% | 192.2% | 139 |
| | | 500 | 3/3 | 500 | 0.964 | 157.5% | 251.1% | 185 |
| | | 1000 | 3/3 | 1000 | 0.962 | 236.2% | 280.0% | 258 |
| SRT | **50** | 50 | 3/3 | 50 | 0.989 | 155.2% | 193.4% | 128 |
| | | 100 | 2/3 | 57 | 0.905 | 190.4% | 251.3% | 166 |
| | | 150 | 1/3 | 81 | 0.000 | 216.4% | 262.8% | 209 |
| | | 200 | 0/3 | 31 | 0.000 | 239.8% | 273.9% | 300 |
| HLS PUT | **≥ 1000** (cap) | 50 | 3/3 | 50 | 1.000 | 23.0% | 45.0% | 139 |
| | | 100 | 3/3 | 100 | 1.000 | 32.4% | 80.0% | 143 |
| | | 250 | 3/3 | 250 | 1.000 | 52.5% | 98.1% | 180 |
| | | 500 | 3/3 | 500 | 1.000 | 76.8% | 118.0% | 252 |
| | | 1000 | 3/3 | 1000 | 1.000 | 117.8% | 165.8% | 383 |

HLS PUT ratio is the share of due segments received. Segment lag behind the
first output: p99 0.7–0.9 s at 500 outputs and 1.1–1.9 s at 1000 (worst
single segment 2.7 s, inside the 3 s budget), so ~1000 is near HLS's limit
on these cores.

Per output, roughly: RTMP ~0.13%, RTMPS ~0.22%, HLS PUT ~0.10%, SRT ~2.9% of
a core (SRT at distinct-enough destinations; see the sink-port note in
[Run it](#run-it)). Past its limit SRT degrades for many destinations at
once rather than a few. SRT's per-output cost is srt-rs per-packet protocol
work; see the [media copy audit](media-copy-audit.md#srt-rs-backlog-evidence-backed).
Every CPU figure includes the harness's per-second health and telemetry
polling ([runtime crossings](runtime-crossings.md) O2).

Earlier ramps (bench profile at `6616fa85`; release at `b4159089` and
`43b68dc0`, uncapped ladders) are superseded. They also showed that
`RTMP×4000` failed on the command-admission bug fixed in `85a6699f`, and that
single-port SRT sinks understated SRT CPU by about a third.

**Comparing runs.** On this shared KVM VPS the same binary's CPU at the same
rung moved by ~30% between sessions (RTMPS×500: 124% in the baseline ramp,
158% in a later session for both baseline and current). Compare builds only
with interleaved A/B runs in one session; treat cross-session capacity
tables as indicative. The baseline numbers above used the bench profile;
evidence committed from now on uses release binaries
(`scripts/build/release-harness.sh`).

## Prompt for running on another machine

Give this to an agent (or follow it by hand) on the target machine:

```text
Goal: measure Restream egress capacity (RTMP, RTMPS, SRT and HLS PUT fan-out
from one 8 Mbit/s ingest) on this machine with the repository's reproducible script,
and report results comparable to the reference host in docs/capacity-ramp.md.

1. Clone https://github.com/krsna1729/restream and check out the agreed
   commit: 6549358b (the measured ramp code of 1a2e7e12 plus a CI build fix).
   Do not modify tracked files. If you must change
   anything to get it running, stop and report the exact change and reason.
2. Set up the toolchain: scripts/dev/bootstrap.sh (Debian/Ubuntu) then
   scripts/dev/prepare.sh. Record the OS, kernel (uname -a), `lscpu`,
   `lscpu -e`, `free -g`, and whether the host is bare metal or a VM
   (`systemd-detect-virt`).
3. Host prep: sudo modprobe tls; sudo sysctl -w net.core.somaxconn=4096
   net.core.rmem_max=26214400 net.core.wmem_max=26214400; ensure
   `ulimit -n` can reach 65536. Make sure nothing else runs media (no
   restream, mediamtx or ffmpeg processes) and the host is otherwise idle.
4. Read docs/capacity-ramp.md fully, then choose the CPU split:
   - More than one NUMA node: CAPACITY_RESTREAM_CPUS = all CPUs of node 0,
     CAPACITY_HARNESS_CPUS = all CPUs of node 1.
   - One node: leave the defaults (half and half).
5. Run A (product defaults; ladders stop at 1000 outputs per protocol):
     scripts/harness/capacity-ramp.sh
   Compare hosts by CPU per output at equal rungs, not only by capacity:
   a protocol that passes 1000 on both hosts is capped, not equal.
   Run B: same ladders plus CAPACITY_EGRESS_SHARDS=<number of Restream CPUs>.
   Run C (malloc arena qualification; Restream's arena cap of 2 is
   provisional): with Run A's shard setting, repeat each protocol at two
   representative rungs (the highest rung that passed in Run A and half of
   it), plus CAPACITY_PROTOCOLS=transcode at its default ladder, once per
   CAPACITY_MALLOC_ARENA_MAX in: default, 2, 4, 8. Report CPU, RSS and
   delivery for every combination; the startup log line
   `restream.malloc.arenas` in each rung's restream log records what was
   applied.
   During the highest passing and first failing rung of each protocol,
   sample `top -b -n 3 -d 5` to record Restream vs test_harness CPU. If the
   test_harness process is at its CPU budget, the receiver is the limit:
   rerun that protocol with CAPACITY_PEER_COUNT=4 and say so.
6. Report, for each run: the full summary.md, provenance.json, the per
   protocol capacity, CPU per output at 1000 RTMP / 1000 RTMPS / 1000 HLS
   PUT / 100 SRT outputs (or the closest passing rung), HLS segment lag
   p99/max (each rung's resource-sweep-results.json `delivery.hls`), how SRT
   failed at its limit
   (graceful vs collapse), any harness failures with the last 50 lines of the
   rung's harness.log, and the top samples from step 5. Do not summarize
   away failed repeats.
```
