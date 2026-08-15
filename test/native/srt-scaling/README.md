# SRT scaling investigation tools

Standalone C benchmarks used to investigate SRT connection-fan-in scaling
against stock libsrt, TCP, and raw UDP, isolated from restream/mediamtx/
Tokio entirely. Manual investigation tools, not part of the automated test
suite or CI — build and run on demand. Full writeup and status:
[`docs/agent-guidance/quality/srt-scaling-investigation.md`](../../../docs/agent-guidance/quality/srt-scaling-investigation.md).

## Contents

- [Build](#build)
- [Tools](#tools)
- [sweep.sh](#sweepsh)
- [What's still open](#whats-still-open)

## Build

```sh
scripts/build/native-deps.sh   # once, if not already built
test/native/srt-scaling/build.sh
```

## Tools

- `sender_bench.c` / `sink_bench.c` — SRT tier, stock (unpatched) libsrt.
  `sink_bench` takes a `port_count` (independent listener ports/multiplexers)
  and `total_worker_threads`; `sender_bench` takes a matching `port_count`
  to spread connections across, plus optional `local_port_count`/
  `local_port_base` to bind outbound sockets to distinct local ports
  (mirrors restream's own `SrtEgressMuxerPorts` sender-side sharding —
  without it the sender itself becomes the bottleneck, not just the
  receiver).
- `tcp_sender.c` / `tcp_sink.c` — TCP control tier, same checkpoint-ramp
  methodology.
- `udp_sender.c` / `udp_sink.c` — raw UDP tier (no ARQ, no encryption, no
  TSBPD — isolates pure kernel/NIC/CPU cost from SRT's protocol overhead).
  `udp_sink` supports two receive-path modes: `shared` (plain UDP sockets,
  the same shape as one SRT multiplexer's kernel socket) and `connected`
  (per-peer `connect()`-isolated sockets, the same kernel-routing mechanism
  validated for libsrt itself in the patched-fork exploration referenced in
  the investigation doc).

All three senders use the same design, converged on after several
iterations that each turned out to be silently wrong in a different way —
worth knowing before extending these tools further:

- **Per-connection deadline pacing, not epoll-driven opportunistic sends.**
  An `epoll_wait()`-on-`SRT_EPOLL_OUT`/`EPOLLOUT` design (send once per
  ready-fd per poll) under-called the send syscall by ~50x at scale: 0
  errors and 0 would-block, just far too few send attempts, because
  write-readiness for a lightly-filled send buffer does not reliably
  re-fire at the cadence a small payload needs.
- **Per-thread exclusive connection ownership, not a shared array filtered
  by owner.** Scanning the full connection array from every thread and
  filtering by `owner_thread` is `Nthreads`x redundant work plus real
  cross-thread cache-line contention — not a host CPU ceiling, easy to
  mistake for one.
- **A timer wheel, not a per-tick linear scan**, once thread counts got low
  enough that O(N) scanning of all owned connections every tick started
  dominating over actual sends (`udp_sender.c` only). All streams share
  nearly the same pacing interval, so a fixed-size ring of time slots gives
  O(1) amortized "what's due now" — no kernel timer (`timerfd`) needed.
- **CPU affinity pinning, sender and receiver on non-overlapping cores**,
  avoiding core 0 (interrupt/softirq/kernel-housekeeping noise on most
  hosts). Running a sender and receiver thread both pinned to the same
  core roughly halved achieved throughput in one measured case.
- **`rdtscp` instead of `clock_gettime()`** in the hottest per-tick timing
  check (`udp_sender.c`'s wheel) — a handful of cycles vs. `clock_gettime`'s
  vDSO call overhead, calibrated against `CLOCK_MONOTONIC` once at startup.

None of this closed the remaining gap to line-rate, though: `perf record -g`
on the most-optimized single-thread sender showed >99% of cycles inside the
kernel's UDP transmit/loopback-delivery path, under 1.3% in application
code. The remaining ceiling on the host this was measured on is genuine
per-packet kernel cost (routing lookup, netfilter, loopback delivery), not
anything fixable by further userspace scheduling changes — closing it
further would need `sendmmsg()`/GSO batching or a different I/O model
(`io_uring`, `AF_XDP`), not more of what's here.

## `sweep.sh`

Sweeps SRT `port_count` at 600/900/1200 real 8Mbps connections, looking for
the smallest port count that gives a genuinely clean result (judge by the
`pct_of_target` column, not just error counts — see the investigation doc
for why an earlier run of this exact idea gave a false-clean answer before
the sender pacing bug above was found). **Not yet re-run to a real
conclusion post-fix** — this is the queued next step, not a finished
result.

```sh
./sweep.sh
# results in sweep-results.csv
```

## What's still open

- The smallest `port_count` for a clean stock-libsrt result at 1,200 real
  8Mbps connections is unresolved. `sweep.sh` is built and ready; it hasn't
  been run to completion with the corrected sender.
- The remaining ~1.7-2.0 Gbps ceiling measured for a single-core-pinned
  UDP sender/receiver pair (see the investigation doc's "final Gbps"
  numbers) has not been pushed further with `sendmmsg()`/GSO batching.
- `udp_sender.c`'s timer wheel and `rdtscp` timing were never backported to
  `sender_bench.c` (SRT) or `tcp_sender.c` — those two still busy-spin with
  a plain per-tick scan of their owned connections. At the thread counts
  used so far (6 threads, ~200 owned connections each) that scan was not
  shown to be the bottleneck, but it wasn't ruled out either.
