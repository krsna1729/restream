# SRT Fan-In Scaling Investigation — 2026-08-15

## Contents

- [Summary](#summary)
- [Background](#background)
- [Method: isolated libsrt benchmarking, real 8Mbps](#method-isolated-libsrt-benchmarking-real-8mbps)
- [Results (superseded — see correction below)](#results-superseded--see-correction-below)
- [Correction: the sender benchmark was silently under-attempting](#correction-the-sender-benchmark-was-silently-under-attempting)
- [Corrected TCP vs UDP vs SRT ladder](#corrected-tcp-vs-udp-vs-srt-ladder)
- [What restream already had right](#what-restream-already-had-right)
- [What was folded back into restream](#what-was-folded-back-into-restream)
- [Patched-libsrt exploration (documented, not adopted)](#patched-libsrt-exploration-documented-not-adopted)
- [Pure-Rust SRT design proposal (research artifact, not adopted)](#pure-rust-srt-design-proposal-research-artifact-not-adopted)
- [Live verification](#live-verification)
- [What remains open](#what-remains-open)

## Summary

**This doc was corrected after its first version overclaimed two results.**
Both errors were found by continued questioning after the fact, not caught
during the original work — see
[Correction: the sender benchmark was silently under-attempting](#correction-the-sender-benchmark-was-silently-under-attempting)
and the `srt-only` bullet in [Live verification](#live-verification) for the
full account. In short:

- The original claim that `port_count=4` gives a "clean" result at 1,200
  real 8Mbps SRT connections was based on a sender with a pacing bug that
  under-called `sendmsg()` by roughly 50x — "0 errors" meant "barely
  attempted to send," not "delivered cleanly." Fixed, but the port-count
  question itself is **unresolved as of this correction** — see
  [Corrected TCP vs UDP vs SRT ladder](#corrected-tcp-vs-udp-vs-srt-ladder).
- The original claim that `srt-only`@1,200 "passed cleanly 3/3 times" in
  the live msr harness was true only in the narrow sense that the harness's
  pass/fail gate doesn't check real throughput. Actual delivered bitrate
  was ~5.6% of target with 10-15 million dropped packets per run — the
  mix was badly degraded, the harness just didn't measure that.

A gist cataloguing suspected libsrt scaling weaknesses prompted a
re-investigation of whether restream's SRT egress problems at 1,200 concurrent
outputs (see
[`srt-egress-scale-investigation-2026-08-10.md`](srt-egress-scale-investigation-2026-08-10.md))
were symptoms of a deeper libsrt architecture limit. Isolated C benchmarks
against stock (unpatched) libsrt — bypassing restream, mediamtx, and Tokio
entirely — confirm the gist's qualitative claim (a shared libsrt multiplexer
does degrade under real sustained 8Mbps-per-connection load), but the specific
"`port_count=4` is clean" number does not hold under a corrected, fair
measurement — see the correction section for what's now known instead: a
real TCP-vs-UDP-vs-SRT throughput ladder, and a `perf`-profiled root cause
for the remaining ceiling.

A separate patched-libsrt line of work (bounded thread pools + `connect()`-based
per-peer kernel isolation, pushed to a public fork) explored fixing this inside
libsrt itself. It resolved two real concurrency bugs along the way but did not
close the 900–1,200-connection gap, and per explicit redirect was deprioritized
in favor of investigating stock libsrt harder instead. That fork and an
accompanying pure-Rust SRT design proposal remain as reference material, not
production changes — see the two sections below.

Live-verified against the actual msr harness: `canonical`@1,200 passes
cleanly. `srt-only`@1,200 reported clean too, but see the correction above —
that result doesn't mean what it first appeared to mean.

The C-benchmark tools themselves (fixed, and further hardened with a
timer-wheel scheduler, CPU-affinity pinning, and `rdtscp`-based timing) are
now checked into `test/native/srt-scaling/` for whoever picks up the
still-open port-count question — see that directory's `README.md`.

What actually shipped to restream from this investigation is narrow: the
`--sink-mode` test-harness listener (`src/media/srt/listener.rs`) was missing
the same high-bitrate UDP/SRT buffer preset the production ingest listener two
paragraphs below it in the same file already applies, and two env vars that
were misnamed after mediamtx even though they control either peer backing
(`MTX_COUNT`, `MTX_SKIP_START`) were renamed to `PEER_COUNT`/`PEER_SKIP_START`
— see [What was folded back into restream](#what-was-folded-back-into-restream)
for why `PEER_*`, not `SINK_*`. Production SRT egress (`SrtEgressMuxerPorts`,
per-shard multiplexer sharding) and production SRT ingest (`buffer_sizing.rs`
+ `srt_set_highbitrate_opts`) already had adequate fixes in place before this
investigation started; neither needed to change.

## Background

The gist (author-supplied, accuracy unverified beyond what's independently
confirmed here) argued that most of restream's SRT scaling pain is a **receive/
fan-in-side libsrt limitation**, bypassable by spreading connections across more
listener ports (more multiplexers, more threads) rather than chasing it as an
application-level bug. This reframed weeks of egress-side investigation
(connect timeouts, shard formulas, connect-admission control — all real and
already fixed, see the 2026-08-10 doc) as treating symptoms of a receive-side
libsrt ceiling that no amount of sender-side tuning removes.

The directive that followed: step back from application-level tuning, prove or
disprove the fan-in ceiling against **stock, unpatched libsrt** with **real
8Mbps sustained traffic** (matching restream's actual MSR fixture bitrate, not
a lighter synthetic rate), and find the smallest port count that gives a clean
1,200-connection result — leveraging (but not blocking on) a parallel patched-libsrt
exploration and Rust-crate research already underway.

## Method: isolated libsrt benchmarking, real 8Mbps

This work started in `.local/experiments/srt-scaling/` (git-ignored, outside
the repository); the benchmark tools (in their final, corrected form — see
below) now live in `test/native/srt-scaling/`, built against the same static
`libsrt.a` restream links via `test/native/srt-scaling/build.sh`. Key tool:
`sweep.sh`, driving `sink_bench.c` (stock libsrt, `SRTO_UDP_RCVBUF` = 192MB
"tuned" value, clamped by `net.core.rmem_max`) and `sender_bench.c` (stock
libsrt, `SRTO_MAXBW` = 1,000,000 B/s = exactly 8Mbps per connection) through
a 600/900/1,200-connection checkpoint ramp.

- `port_count` ∈ {2, 4, 8}: number of independent listener ports/multiplexers
  the sink opens; senders spread evenly across them.
- 5 repetitions per cell, host-load-gated (`/proc/loadavg` ceiling + a
  `pgrep`-based liveness check between cells, so no cell starts on a host still
  cooling down from the previous one).
- Bitrate: `BITRATE=1000000` bytes/sec = 8,000,000 bits/sec = 8Mbps exactly,
  matching restream's real 1080p60 MSR fixture, not a lighter placeholder rate.

## Results (superseded — see correction below)

| `port_count` | total `steady_send_errors` (5 reps) | total failed connections (5 reps) |
|---|---|---|
| 2 | 9,334 | 232 |
| **4** | **0** | **0** |
| 8 | 0 | 0 |

This table was the original conclusion: `port_count=4` looked like the
smallest clean configuration. **It is not trustworthy** — see the next
section. `sender_bench.c`'s pacer under-called `srt_sendmsg2()` by roughly
50x at every one of these cells, so "0 errors" mostly meant "hardly
attempted to send," not "delivered cleanly." Left in place (not deleted) so
the correction below is legible against what it's correcting.

## Correction: the sender benchmark was silently under-attempting

Found by direct questioning after the table above was already written and
believed: does `steady_bytes_sent` at `port_count=4`/1,200 actually
approach the target aggregate (1,200 × 8Mbps = 9.6Gbps)? It did not —
`steady_bytes_sent` at that cell implied only ~1.9% of target delivered.
`sender_bench.c` and `sink_bench.c` never called `srt_bstats`/checked any
loss counters; they only tracked `srt_sendmsg2()` return codes. The original
sender used `srt_epoll_wait()` on `SRT_EPOLL_OUT` and sent once per
ready-fd per poll — write-readiness for a lightly-filled `SNDBUF` doesn't
reliably re-fire at the cadence a 1,316-byte payload needs, so the loop
called `sendmsg()` far too infrequently. Neither `send_errors` nor
`send_would_block` caught this because the calls that *did* happen mostly
succeeded — the bug was in call frequency, not call failure.

A second bug compounded it once the first was partially fixed: the sender
never bound its outbound sockets to distinct local ports before
`srt_connect()`, so libsrt's own multiplexer-reuse logic (`updateMux()`)
could — and did — consolidate all 1,200 client-side sockets onto one shared
local multiplexer, the same shared-`CSndQueue` bottleneck shape as the
well-known listener-side one, just on the send side. Binding each
connection to one of a small pool of local ports (mirroring restream's own
`SrtEgressMuxerPorts` sender-side sharding) jumped delivery from ~1.9% to
~22.8% of target at the same `port_count=4`/1,200 cell — confirming this was
real and significant, not noise.

With both bugs fixed, `port_count=4` at 1,200 connections shows **real**
`send_errors` (order of 2-3 million) and delivers roughly 17-26% of target
across repeated runs — genuinely worse-looking than the original "0 errors"
table, because the original table was measuring almost nothing. The
port-count sweep was never re-run to completion with the corrected sender
(see [What remains open](#what-remains-open)); `test/native/srt-scaling/sweep.sh`
is built and ready for whoever picks this up.

## Corrected TCP vs UDP vs SRT ladder

With the sender fixed, plus per-thread-exclusive connection ownership
(replacing an interim design that had every thread scan a shared array
filtered by `owner_thread` — `Nthreads`x redundant work and real
cross-thread cache contention, easy to mistake for a host CPU ceiling) and
CPU-affinity pinning (sender and receiver processes both independently
pinning thread 0 to CPU 0 by default cost roughly half of achieved
throughput in one measured case — core 0 tends to carry interrupt/softirq/
kernel-housekeeping noise on most hosts), a three-way structural comparison
at the 1,200-connection checkpoint (`port_count=4` where applicable):

| tier | achieved | % of 9.6Gbps target |
|---|---|---|
| Raw UDP, shared socket (6 threads, 4 ports) | ~6.0 Gbps | 62% |
| Raw UDP, `connect()`-isolated (6 threads, 4 ports) | ~5.0 Gbps | 52% |
| TCP (6 threads) | ~3.5 Gbps | 36% |
| SRT (6 threads, 4 ports) | ~1.6-2.5 Gbps (noisy across runs) | 17-26% |

Raw UDP (no ARQ, no encryption, no TSBPD, no congestion control) clearly
outperforms both TCP and SRT under identical threading. TCP's kernel
per-connection state/ACK bookkeeping costs more than naive intuition
suggests relative to UDP's fire-and-forget path. SRT sits well below all
three and is the *only* tier with genuine backpressure errors — TCP/UDP's
kernel-level flow control degrades silently, SRT's userland flow-control
window explicitly rejects sends once the receiver falls behind. Since raw
UDP uses the identical thread/socket architecture as SRT and does 2.5-4x
better, that gap is honestly attributable to libsrt itself (ARQ bookkeeping,
flow-control window checks, `SRTO_MAXBW` pacing math, TSBPD) — not to
shared host limits, which hit all three tiers as a common floor.

**How far the floor itself goes down**, isolating threading/scheduling
entirely: two additional rounds of tuning `udp_sender.c`/`udp_sink.c` —
replacing a per-tick O(N) scan of owned connections with a userspace timer
wheel (one rotation ≈ one pacing interval, so a connection lands back in
roughly the same time-slot every lap; no `timerfd`, no per-connection heap),
switching `clock_gettime()` for `rdtscp` in the hot path, busy-spinning
instead of `nanosleep()` (this is a VM — sub-millisecond `nanosleep()`
requests reliably overshoot far more than on bare metal), and finally
pinning the sender and receiver to **different**, non-zero cores (1 and 2)
instead of both independently defaulting to core 0 — brought a single
TX-thread/RX-thread pair to:

| checkpoint | achieved |
|---|---|
| 600 connections | 1.99 Gbps |
| 900 connections | 1.83 Gbps |
| 1,200 connections | 1.68 Gbps |

Still nowhere near the 9.6Gbps a naive "one core should trivially saturate
this" expectation predicts. `perf record -g` on that same sender resolved
it conclusively: **>99% of all sampled cycles were inside `__send`**,
deep in the kernel's UDP transmit/loopback-delivery path (routing lookup,
netfilter, `loopback_xmit`, softirq delivery into the receiver's queue) —
under 1.3% in application code. Every round of userspace optimization above
had already succeeded at making the application side nearly free; the
remaining ceiling on this host is genuine **per-packet kernel networking
cost**, not anything fixable by further scheduling changes. Closing it
further needs `sendmmsg()`/GSO batching (amortizes syscall entry/exit, but
routing/netfilter still runs per-packet inside the kernel either way) or a
different I/O model (`io_uring`, `AF_XDP`) — out of scope for this
investigation, noted as future work.

The C benchmark tools (all of the fixes above) are checked into
`test/native/srt-scaling/` — see that directory's `README.md` for the full
list of design mistakes each fix corrected, useful context for anyone
extending or trusting output from these tools later.

## What restream already had right

Two production code paths were audited against this finding and found already
adequate — neither was changed:

- **SRT egress** (`src/media/srt/socket.rs`, `SrtEgressMuxerPorts` in
  `src/media/egress/backends/srt/muxer_ports.rs`): already shards egress
  connections across N multiplexers sized by CPU count, per pipeline. This is
  the send-side analog of the port-multiplication fix and was already in place
  before this investigation (see `338b94aa`/`f08a4a97`/`641288a8` in this PR's
  own history).
- **SRT ingest** (`src/media/srt/listener.rs`'s production listener,
  `src/media/srt/buffer_sizing.rs`): already calls `srt_set_highbitrate_opts`
  with the configurable `srt_udp_buffer` (`RESTREAM_SRT_UDP_BUFFER`, default
  8MB) before bind, and separately latency-scales `SRTO_RCVBUF`/`SRTO_FC` per
  pipeline. This matches "ingest path exonerated" from the earlier
  investigation (`b60b91e2`) — real publisher ingest was never the bottleneck
  this campaign was chasing.

## What was folded back into restream

**`src/media/srt/listener.rs`, `--sink-mode` discard listener.** This is the
one gap: the test-harness receive path used by the msr scale harness
(`MSR_PEER=sink`, up to 1,200 simultaneous accepted connections against one
listener when `PEER_COUNT=1`, the default) built its socket with only
`SRTO_TRANSTYPE`/`SRTO_LATENCY`/`SRTO_REUSEADDR`/`SRTO_RCVSYN` set — no UDP or
SRT buffer tuning at all, unlike the production ingest listener 70-odd lines
below it in the same file, which has called `srt_set_highbitrate_opts` for as
long as that function has existed. Added the same call
(`srt_set_highbitrate_opts(server_sock, self.engine.config.srt_udp_buffer as
i32)`) to the sink-mode path, before bind, preserving the existing 250ms
latency override immediately after. This is a straight buffer-size increase
on a receive-dominant socket, applied to bring the test harness's own
receiver up to the same standard restream already holds its production
listener to — a defensible change on parity grounds alone, independent of
the isolated-benchmark port-count conclusion this doc had to retract (see
the correction section): a larger receive buffer cannot make things worse,
whatever the real port-count answer turns out to be.

**`MTX_COUNT`/`MTX_SKIP_START` → `PEER_COUNT`/`PEER_SKIP_START`.** Both env
vars control behavior for either `MSR_PEER=mediamtx` (real mediamtx
processes) or `MSR_PEER=sink` (`restream --sink-mode` processes) —
`spawn_sink_peer` reads `MTX_SKIP_START` exactly like `spawn_mediamtx_peer`
does — so `MTX_`-prefixing both was a misnomer once the sink peer mode
existed. The first attempt at this rename used `SINK_COUNT`, since most real
scale-test usage is sink mode, not mediamtx — but `core/ports.rs` already has
an unrelated, pre-existing `SINK_PORT`/`.sink` field (a generic RTMP-sink
test-server port range used across `fault_recovery/egress.rs`,
`live_modes/protocol.rs`, `fault_recovery/resilience.rs`,
`mixed_live_scenarios.rs` — nothing to do with this mediamtx-vs-sink-mode
peer choice), so `SINK_COUNT` would have reused "sink" for a second, unrelated
meaning in the same binary. Switched to `PEER_*` instead, matching the
vocabulary the codebase already uses for exactly this generic concept
(`MSR_PEER`, `ResourceSweepPeer`, `peer_mode`, `spawn_mediamtx_peer`/
`spawn_sink_peer`). Renamed the env vars, the `ResourceSweepEnv` field
(`mtx_count`→`peer_count`), and every doc/code reference across
`resource_sweep.rs`, `resource_sweep/config.rs`, `resource_sweep/msr.rs`,
`resource_sweep/msr/plan.rs`, `resource_sweep/msr/tests.rs`,
`mediamtx_probe.rs`, and `docs/mahashivratri-hero-scenario.md`.

Left `MTX_RTMP`/`MTX_SRT`/`MTX_HLS`/`MTX_API` (and the matching
`mtx_rtmp`/`mtx_srt`/`mtx_hls`/`mtx_api` fields) untouched despite also being
read by `spawn_sink_peer` for its port numbers: `core/setup.rs`'s
`MEDIAMTX_CONFIG_ENV_NAMES`/`remove_mediamtx_config_env` strips exactly these
four names from every spawned mediamtx child's environment across the
harness, which only makes sense if they double as (or risk being mistaken
for) mediamtx's own native config-override env vars — renaming them would
mean guessing at that external contract without being able to verify it
against mediamtx's actual source, an unfavorable risk for a cosmetic rename.
`MTX_RTMPS`/`MTX_HLS` specifically are also genuinely mediamtx-only in
function (TLS RTMP and HLS serving; sink mode has neither), reinforcing that
these four are a different case from `MTX_COUNT`/`MTX_SKIP_START`, which
appear nowhere in that scrub list because they have no such external meaning.
`srt-egress-scale-investigation-2026-08-10.md` was intentionally **not**
rewritten — it is dated evidence describing what was true when it was
written, under the names that existed then.

No change was made to give sink-mode multiple listener ports within one
process (the direct analog of the C benchmark's `port_count` knob). The
harness already has that capability at a coarser grain — `PEER_COUNT>1`
spawns N separate `restream --sink-mode` *processes*, each with its own
libsrt instance and kernel socket set, a stronger form of isolation than N
ports in one process. Building N-ports-per-process inside sink-mode itself
would duplicate that capability for a test-only receiver with no production
counterpart to justify the added complexity (see `docs/agent-guidance/skills/layering-audit/SKILL.md`
on stopping when the next split adds more indirection than ownership
clarity).

## Patched-libsrt exploration (documented, not adopted)

A parallel line of work modified libsrt itself to add bounded thread pools
(`CRcvQueuePool`/`CSndQueuePool`/`CTsbpdPool`, replacing one dedicated
thread-pair per accepted connection with a small pool of threads owning many
connections each) plus `connect()`-based per-peer kernel isolation
(`SO_REUSEADDR` + `connect()` on the accepted socket, verified against Linux
v6.8's `udp_lib_lport_inuse()`/`__udp4_lib_lookup()` to route each peer's
traffic to its own connected kernel socket without needing `SO_REUSEPORT`,
which does not work for SRT — see haivision/srt issues #1324/#2343).

Two real concurrency bugs were found and fixed during that work: a
dangling-pointer crash (a naive locking scheme had no removal path at all,
confirmed via `dmesg` segfaults once connection churn exercised it past 600
connections) and, after the crash fix, a lock-contention stall (the fix's
single per-sweep mutex serialized `add()`/`remove()` against the full sweep
duration, dropping accept throughput to 19/300 connections in 8 seconds). The
final design is thread-confined: each pool thread owns its connection list
exclusively; other threads communicate only through a small, briefly-locked
inbox drained at the start of each loop iteration. This resolved both bugs,
confirmed via a full 600/900/1,200 chaos-adjacent ramp with bounded thread
counts throughout — but did **not** close the 900–1,200-connection
degradation gap (148/300 new connections still failed handshake at 1,200
under the patched code). Two follow-up attempts (more pool threads, an
epoll-based dispatch variant) both made it measurably worse and were reverted;
current best explanation is genuine CPU-bound saturation processing ~9.6Gbps
aggregate across 1,200 separate kernel-socket contexts on a 6-core host, not a
scheduling or locking artifact.

Pushed as 5 logically-grouped commits to
[`krsna1729/srt`](https://github.com/krsna1729/srt), branch `scaling`, based
on v1.5.5 (`b6b4ae99`). Verified to build cleanly from a fresh checkout before
pushing. **Not adopted into restream** — per explicit redirect, attention
moved to investigating stock libsrt (more listener ports, no custom fork)
instead, on the assumption that would be simpler and sufficient. Whether it
actually is remains open (see the correction section and
[What remains open](#what-remains-open)) — the fork stays as reference
material either way, and is the starting point if the stock-libsrt path
turns out not to be enough.

## Pure-Rust SRT design proposal (research artifact, not adopted)

`.local/experiments/srt-scaling/rust-srt-design.md` (git-ignored, not part of
this repository) is a 10-part design proposal for a from-scratch, sans-I/O
Rust SRT implementation — protocol state machine, connection lifecycle, and
threading model as three separable layers, so the application (not libsrt)
owns thread/socket placement. It surveys two existing Rust SRT crates
(`russelltg/srt-rs`: mature 5-crate layering, differential-tested against
libsrt's own unit tests, but stale since mid-2024 and self-flagged
not-production-ready; `shiguredo/srt-rs`: younger, genuinely sans-I/O,
LIVE-mode-scoped matching restream's usage, but excludes group/bonding
support — restream's one real gap against that crate's current scope, since
restream actively uses SRT bonding on both ingest and egress) and audits
restream's full `SRTO_*`/`srt_*` FFI dependency surface against both.
Forward-looking research only; no code from it has been adopted. It is not
committed to the repository because it lives entirely outside `src/`, `docs/`,
or any tracked path — anyone continuing this line of work should copy the
relevant sections into a tracked doc first.

## Live verification

`--sink-mode`'s buffer fix and the `PEER_COUNT` rename were verified against
the actual msr live harness, not just the isolated C benchmarks above:

- `cargo build --profile bench` (both `restream` and `test_harness` binaries)
  and the 19 existing `resource_sweep`/`msr` unit tests all pass unchanged
  after the rename.
- `MSR_OUTPUT_COUNTS=1200 MSR_PEER=sink MSR_PROTOCOL_MIX=canonical
  scripts/harness/run.sh msr -- --no-netns`: **PASS**, `outputs=1200/1200` in
  1s, zero errors/warnings in the harness log. This is the mix the existing
  [netns-confound doc](msr-1200-netns-confound-investigation-2026-08-14.md)
  proved is informative even without real network-namespace isolation, so
  this is a genuine regression check for the sink-mode change, not an
  uninformative run.
- `MSR_OUTPUT_COUNTS=1200 MSR_PEER=sink MSR_PROTOCOL_MIX=srt-only
  scripts/harness/run.sh msr -- --no-netns`, **3 repetitions**: harness
  `PASS`, 3/3, `outputs=1200/1200` in 274s, 374s, 303s. **This claim needs a
  major caveat, found after the fact**: the harness's `PASS` only checks
  "all 1,200 outputs present, `bytesOutDelta > 0`" — it does not check
  sustained throughput. Checking the actual sample data these three runs
  produced: `bytesOutDelta` over the 5s sample window was ~336-339MB each
  run (~538-543Mbps aggregate against a 9,600Mbps target — **~5.6% of
  target**), with `packetsSentDrop` of 10.7-15.5 **million** dropped packets
  per run. `restreamCpuAvgPct` sat around 230-243% (well under the 6-core
  ceiling), ruling out sender-side CPU exhaustion — this is the same
  receive-side degradation the C-benchmark work characterizes, just not
  caught by this harness's specific pass/fail gate. The mix was badly
  degraded; the harness only checks "some bytes still trickling to every
  output," which held true throughout. Re-read as: the sink-mode buffer fix
  did not resolve the underlying bottleneck at true full rate — it just
  didn't move this harness's particular assertion. `canonical`@1,200 above
  is unaffected by this caveat (only 5% of its outputs are SRT, and its
  `bytesOutDelta`/CPU profile were not re-checked against this same bar —
  worth doing before trusting it fully either).

## What remains open

- **The smallest `port_count` for a genuinely clean stock-libsrt result at
  1,200 real 8Mbps connections is unresolved.** The original "4" answer is
  retracted (see the correction section). `test/native/srt-scaling/sweep.sh`
  is built, checked in, and ready to re-run to a real conclusion — judge by
  the `pct_of_target` column, not just error counts, this time.
- **`srt-only` at 1,200 reported `PASS` 3/3 times under `--no-netns` in this
  session, but that result is now known not to mean "delivered cleanly"**
  (see the corrected bullet in [Live verification](#live-verification)) —
  actual throughput was ~5.6% of target with millions of dropped packets.
  Whether the sink-mode buffer fix helps at all under a throughput-aware
  measure, and whether the 2026-08-14 netns confound is resolved, are both
  still open. `unshare --net` also remains unavailable in this sandbox.
- **The remaining ~1.7-2.0 Gbps ceiling measured for a single core-pinned
  TX/RX thread pair** (confirmed via `perf` to be genuine per-packet kernel
  cost, not application-level) has not been pushed further with
  `sendmmsg()`/GSO batching or a different I/O model. Future work, not
  started here.
- **The 900–1,200-connection degradation is unresolved in the patched-libsrt
  line of work** and was not chased further once attention moved to
  investigating stock libsrt harder instead. If a future need requires
  *fewer than 4* independent ports at 1,200 real connections, the
  patched-libsrt fork and its thread-confined pool design are the starting
  point, not a fresh rewrite.
- **`udp_sender.c`'s timer wheel and `rdtscp` timing were never backported
  to `sender_bench.c` (SRT) or `tcp_sender.c`** — those two still busy-spin
  with a plain per-tick scan of owned connections. Not shown to be the
  bottleneck at the thread counts used (6 threads, ~200 owned connections
  each), but not ruled out either.
- **`rust-srt-design.md` is not in version control.** If the pure-Rust SRT
  direction gets picked up, move it into `docs/` first so it survives outside
  this one sandbox's `.local/` state.
