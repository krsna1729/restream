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
- **Worst interval ratio**: the lowest one-sample ratio, which exposes stalls
  an average hides.
- **Jain fairness** over destination rates.
- **Restream's own view** (`/api/v1/pipelines/{id}/telemetry` `delivery`):
  RTMP from TCP `bytes_acked`, SRT from ACK coverage (an upper bound; see
  [observability](observability.md#per-destination-delivery)).
- Restream CPU (average and peak, % of one core), RSS and thread count.

**Capacity** for a protocol is the highest rung where every repeat passed.

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

It builds the bench binaries (`scripts/build/bench-harness.sh`), refuses a
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
  Restream, saturates. Check the sink side with `top` during a high rung: the
  `test_harness` process should stay below its CPU budget.
- Extend the ladders on big hosts, for example
  `CAPACITY_RTMP_OUTPUTS=500,1000,2000,4000,8000`
  `CAPACITY_SRT_OUTPUTS=100,200,400,800,1600`. A ladder stops after a rung
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

Baseline: 6-CPU AMD EPYC KVM VPS (1 NUMA node), kernel 6.8, commit
`6616fa85`, `scripts/harness/capacity-ramp.sh` defaults (Restream on CPUs
0–2, harness on 3–5, product-default shards, 30 s windows, 3 repeats per
rung). CPU is % of one core; Restream's budget is 300%.

| protocol | capacity | rung | passed | rx delivered min | rx ratio min | CPU avg | CPU peak | RSS MB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| RTMP | **1000** | 250 | 3/3 | 250 | 0.962 | 60.7% | 115.5% | 184 |
| | | 500 | 3/3 | 500 | 0.959 | 83.6% | 141.1% | 232 |
| | | 1000 | 3/3 | 1000 | 0.964 | 135.3% | 194.9% | 295 |
| | | 2000 | 1/3 | 422 | 0.944 | 216.3% | 259.0% | 452 |
| RTMPS | **1000** | 250 | 3/3 | 250 | 0.963 | 73.5% | 115.9% | 194 |
| | | 500 | 3/3 | 500 | 0.966 | 124.1% | 168.0% | 238 |
| | | 1000 | 3/3 | 1000 | 0.954 | 235.2% | 282.0% | 327 |
| | | 2000 | 0/2 | 0 | 0.343 | 292.5% | 297.6% | 565 |
| SRT | **50** | 50 | 3/3 | 50 | 0.986 | 127.1% | 185.4% | 160 |
| | | 100 | 2/3 | 35 | 0.878 | 168.8% | 253.9% | 204 |
| | | 150 | 0/3 | 0 | 0.000 | 208.2% | 279.8% | 266 |

SRT costs ~2.5% of a core per 8 Mbit/s output (RTMP ~0.14%, RTMPS ~0.24%)
and, past its limit, delivery collapsed for every destination rather than
degrading for a few. The SRT sink stayed within its CPU budget, so Restream
was the limit. Causes and fixes: [media copy audit](media-copy-audit.md#srt-rs-backlog-evidence-backed).

## Prompt for running on another machine

Give this to an agent (or follow it by hand) on the target machine:

```text
Goal: measure Restream egress capacity (RTMP, RTMPS, SRT fan-out from one
8 Mbit/s ingest) on this machine with the repository's reproducible script,
and report results comparable to the reference host in docs/capacity-ramp.md.

1. Clone https://github.com/krsna1729/restream and check out the agreed
   commit: <COMMIT SHA>. Do not modify tracked files. If you must change
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
5. Run A (product defaults), with ladders sized for this host:
     CAPACITY_RTMP_OUTPUTS=250,500,1000,2000,4000,8000 \
     CAPACITY_RTMPS_OUTPUTS=250,500,1000,2000,4000 \
     CAPACITY_SRT_OUTPUTS=100,200,400,800,1600 \
     scripts/harness/capacity-ramp.sh
   Run B: same ladders plus CAPACITY_EGRESS_SHARDS=<number of Restream CPUs>.
   During the highest passing and first failing rung of each protocol,
   sample `top -b -n 3 -d 5` to record Restream vs test_harness CPU. If the
   test_harness process is at its CPU budget, the receiver is the limit:
   rerun that protocol with CAPACITY_PEER_COUNT=4 and say so.
6. Report, for each run: the full summary.md, provenance.json, the per
   protocol capacity, CPU per output at 1000 RTMP / 1000 RTMPS / 200 SRT
   outputs (or the closest passing rung), how SRT failed at its limit
   (graceful vs collapse), any harness failures with the last 50 lines of the
   rung's harness.log, and the top samples from step 5. Do not summarize
   away failed repeats.
```
