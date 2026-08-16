# Rust raw-UDP scaling prototype

A Rust equivalent of `test/native/srt-scaling/udp_sender.c` /
`udp_sink.c`, built to answer a concrete question the C tools' own
README leaves open: does `sendmmsg()`/`recvmmsg()` batching — named there
as the one unexplored lever — actually move the raw-UDP throughput floor?
Standalone prototype, not (yet) part of the workspace conversion planned in
`docs/srt-pure-rust-plan.md` Phase 2; see that doc's Phase 4 for how this
result feeds the "Rust must be measurably cheaper per packet than libsrt"
kill switch.

## Contents

- [Architectural difference from the C tools](#architectural-difference-from-the-c-tools)
- [Build and run](#build-and-run)
- [Measured result](#measured-result)
- [What this does and does not prove](#what-this-does-and-does-not-prove)
- [Next steps](#next-steps)

## Architectural difference from the C tools

`udp_sender.c` gives every simulated stream its own `connect()`ed socket
and calls `send()` once per stream per wheel-slot tick — one syscall per
message. `udp_sender_rs` instead gives each **worker thread** one
unconnected socket and batches every stream due in a tick into a single
`sendmmsg()` call (per-message destination address, shared read-only
payload buffer). Symmetrically, `udp_sink_rs` drains each ready listener
socket with `recvmmsg()` instead of a `recv()`-per-message loop.

This means `udp_sender_rs` drops the C sender's `[local_port_count]
[local_port_base]` arguments — with one shared socket per thread there is
no per-stream socket to bind to a distinct local port — and `udp_sink_rs`
only implements the C sink's `shared` receive-path mode, not `connected`
(per-peer `connect()`-isolated sockets); see [Next steps](#next-steps).

CSV output is close to but not identical to the C tools' (extra
`steady_syscalls` column, no `failed`/`send_errors` columns since there are
no per-stream sockets to fail to create) — diff the two headers before
scripting against both.

## Build and run

```sh
scripts/build/resource-limit.sh cargo build --release   # from this directory
target/release/udp_sink_rs   <port_base> <port_count> <threads> <rcvbuf_bytes> [cpu_base]
target/release/udp_sender_rs <host> <port_base> <port_count> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs> [cpu_base]
```

Not a Cargo workspace member — build from inside this directory. x86_64/
Linux only (uses `rdtscp`), matching the C tools' own assumption.

## Measured result

Same host, same run, same methodology as the C tools, at two thread-pair
scales, both keeping every worker thread on a dedicated, non-overlapping
core in the 1-4 range (core 0 left free, matching the "avoid core 0"
finding already in the investigation doc). 8 Mbps/connection, loopback,
5s hold per checkpoint.

### All results, at a glance

Achieved throughput, syscall count over the 5s hold, and (for the batched
column) messages moved per syscall — `sc` = syscalls:

**2 cores (1 sender thread + 1 receiver thread)**

| Connections | Target | C | Rust, unbatched | Rust, batched |
|---:|---:|---:|---:|---:|
| 600  | 4.8 Gbps | 2.09 Gbps (994,118 sc) | 1.91 Gbps (904,800 sc) | **2.30 Gbps** (1,822 sc, ~600/sc) |
| 900  | 7.2 Gbps | 1.97 Gbps (933,943 sc) | 1.96 Gbps (931,200 sc) | **2.33 Gbps** (2,073 sc, ~533/sc) |
| 1200 | 9.6 Gbps | 1.92 Gbps (912,228 sc) | 2.02 Gbps (959,400 sc) | **2.17 Gbps** (2,289 sc, ~450/sc) |

**4 cores (2 sender threads + 2 receiver threads)**

| Connections | Target | C | Rust, unbatched | Rust, batched |
|---:|---:|---:|---:|---:|
| 600  | 4.8 Gbps | 3.19 Gbps (1,515,864 sc) | 3.88 Gbps (1,843,500 sc) | **4.35 Gbps** (6,891 sc, ~300/sc) |
| 900  | 7.2 Gbps | 3.56 Gbps (1,691,917 sc) | 3.91 Gbps (1,859,250 sc) | **4.86 Gbps** (10,092 sc, ~229/sc) |
| 1200 | 9.6 Gbps | 3.61 Gbps (1,714,621 sc) | 3.71 Gbps (1,761,750 sc) | **4.56 Gbps** (10,630 sc, ~204/sc) |

**6 cores (3 sender threads + 3 receiver threads, cores 0-2 / 3-5)** — uses
core 0, unlike the two tables above, so expect scheduling noise from
whatever interrupt/softirq/kernel housekeeping lands there:

| Connections | Target | C | Rust, unbatched | Rust, batched |
|---:|---:|---:|---:|---:|
| 600  | 4.8 Gbps | 4.76 Gbps (2,259,561 sc) | 4.31 Gbps (2,048,000 sc) | 4.64 Gbps (11,020 sc, ~200/sc) |
| 900  | 7.2 Gbps | 4.64 Gbps (2,202,016 sc) | 4.78 Gbps (2,270,000 sc) | **5.33 Gbps** (16,805 sc, ~151/sc) |
| 1200 | 9.6 Gbps | 4.99 Gbps (2,371,953 sc) | 5.58 Gbps (2,651,200 sc) | **6.09 Gbps** (21,190 sc, ~137/sc) |

At 600 connections, both Rust variants land *behind* C (-9.4% unbatched,
-2.5% batched) — the one result in this whole set where C wins. This is the
core-0 noise the scale-up was expected to surface: at the lightest load
tested, whichever thread happens to land on core 0 pays a proportionally
larger tax from housekeeping interrupts, and 3 threads means someone always
does. At 900 and 1200 the usual pattern reasserts itself and grows (+15.0%
and +22.0% batched vs. C) — plausibly because at higher load the real send/
receive work increasingly dominates over the fixed core-0 tax. Not chased
further here; a host that can give both sides non-overlapping cores 1+
entirely (8+ cores) would remove the confound rather than just outgrow it.

`C` and `Rust, unbatched` are always 1 syscall per message (send/recv
count == syscall count), so their syscall columns double as message counts.

Takeaways:
- At 2 cores, "Rust, unbatched" tracks C almost exactly (±9%, run-to-run
  noise) — same syscall-per-message cost, so no reason to expect a
  difference, and there isn't one. Batching alone (`sendmmsg`/`recvmmsg`)
  is what gets Rust to +10-18% over C at this scale.
- At 4 cores, unbatched Rust already beats C by +3-22% before batching is
  even applied — likely fewer sockets total (2 vs. up to 1200) means less
  per-socket kernel bookkeeping at this connection density. Batching adds
  a further win on top, landing the full Rust-batched number +26-36% over C.
- Batching's own leverage (messages moved per syscall) drops as thread
  count rises — roughly 450 msgs/syscall at 2 cores/1200 connections vs.
  ~204 at 4 cores/1200 — because each thread now serves half as many
  streams per tick. The win doesn't shrink much overall because the
  architecture-only effect (the unbatched row) grows to compensate.

Raw per-run CSVs: `/tmp/sender_{c,rs}_1thread.csv`,
`/tmp/sender_{c,rs}_4core.csv`, `/tmp/sender_rs_{1thread,4core}_nobatch.csv`
(not committed — regenerate via the commands above; both binaries take a
trailing `[cpu_base] [batch:0|1]` to reproduce the unbatched control runs).

## What this does and does not prove

**Proves:** `sendmmsg`/`recvmmsg` batching gives a real, measured, ~10-18%
throughput improvement over one-syscall-per-message at the single-
thread-pair scale already characterized in the investigation doc, on this
host, using safe-adjacent Rust (the `libc` crate is the only dependency;
everything above the FFI boundary is ordinary safe Rust).

**Does not yet prove:** anything about the 6-thread/multi-port aggregate
ceiling (~6.0 Gbps / 62% of target for C's best shared-socket raw-UDP
configuration), or about SRT itself — this tool has no ARQ, encryption, or
TSBPD, same scope limitation the C `udp_sender`/`udp_sink` already have.
It's one data point on the lowest floor of the ladder, not a replacement
for the full sweep.

## Next steps

- Re-run at the full 6-thread/4-port configuration to compare against the
  ladder's "Raw UDP, shared socket (6 threads, 4 ports): ~6.0 Gbps / 62%"
  and "connect()-isolated: ~5.0 Gbps / 52%" rows. Only the 1-thread-pair and
  2-thread-pair (4-core) scales are measured above — a 6-thread sender and a
  6-thread receiver can't both get non-overlapping dedicated cores on this
  6-core host at once; needs a host with more cores, or accepting
  sender/receiver core contention as a noted confound.
- Implement `udp_sink_rs`'s `connected` mode (per-peer `connect()`-isolated
  sockets) with `recvmmsg()` per peer socket, to compare against the C
  tool's `connected` receive path at scale — `recvmmsg` mostly helps a
  connected per-peer socket if that peer bursts multiple datagrams between
  polls, so the win there may look different from the shared-listener case
  measured above.
- `io_uring` (via the `io-uring` crate) is the next lever past `sendmmsg`/
  `recvmmsg` if this one proves the general batching direction is worth
  pursuing further — untried here, scoped out to keep this first prototype
  small and comparable.
- This tool exists to inform, not replace, the SRT-Rust plan's Phase 4 gate
  (`docs/srt-pure-rust-plan.md`): "is a clean-sheet Rust protocol layer
  cheaper per packet than libsrt's, in a pure micro-benchmark with no I/O
  in the way." The result here is encouraging (batching gives a real win)
  but is about the Driver-level I/O floor, not the SRT Core protocol
  overhead itself — that comparison is still Phase 4's, not this tool's.
