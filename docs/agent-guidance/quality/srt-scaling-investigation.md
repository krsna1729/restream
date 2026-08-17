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
- [Pure-Rust SRT design proposal (now committed, plan active)](#pure-rust-srt-design-proposal-now-committed-plan-active)
- [Live verification](#live-verification)
- [Harness-native sink extraction and re-verification — 2026-08-15](#harness-native-sink-extraction-and-re-verification--2026-08-15)
- [Exclusive ports-per-thread pool — fixes the thread-scaling regression](#exclusive-ports-per-thread-pool--fixes-the-thread-scaling-regression)
  - [Exact knee location: 100-step ramp at the pool optimum](#exact-knee-location-100-step-ramp-at-the-pool-optimum)
- [Tuple affinity and bonding identity — 2026-08-17](#tuple-affinity-and-bonding-identity--2026-08-17)
- [Fourth sink topology: one port per stream — 2026-08-17](#fourth-sink-topology-one-port-per-stream--2026-08-17)
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
that result doesn't mean what it first appeared to mean. `RESTREAM_SINK_MODE`
was subsequently removed from production `restream` entirely and replaced
with an in-process, harness-native sink listener — the same `srt-only`
degradation was reproduced (and its `PEER_COUNT` tunable quantified) against
the new implementation; see
[Harness-native sink extraction and re-verification](#harness-native-sink-extraction-and-re-verification--2026-08-15).

The C-benchmark tools themselves (fixed, and further hardened with a
timer-wheel scheduler, CPU-affinity pinning, and `rdtscp`-based timing) are
now checked into `test/native/srt-scaling/` for whoever picks up the
still-open port-count question — see that directory's `README.md`.

What actually shipped to restream from this investigation: `RESTREAM_SINK_MODE`
was removed from production `restream` entirely (it existed only to serve the
msr harness and collided in name with the unrelated, real `sink://` egress
type) and replaced by an in-process, harness-native listener
(`src/bin/test_harness/harness_srt_sink.rs` for SRT, the existing
`sinks.rs` for RTMP) — see the harness-native section below. Two env vars
that were misnamed after mediamtx even though they control either peer
backing (`MTX_COUNT`, `MTX_SKIP_START`) were renamed to
`PEER_COUNT`/`PEER_SKIP_START` — see
[What was folded back into restream](#what-was-folded-back-into-restream)
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

**Historical note**: the `--sink-mode` discard listener described in this
section below was later removed from `src/media/srt/listener.rs` entirely
and replaced by an in-process, harness-native listener — see
[Harness-native sink extraction and re-verification](#harness-native-sink-extraction-and-re-verification--2026-08-15).
The buffer-tuning fix described here was carried forward into the new
`harness_srt_sink.rs` (`apply_highbitrate_opts`, applied from the start
rather than added after the fact) — kept below as the historical record of
why it mattered.

**`src/media/srt/listener.rs`, `--sink-mode` discard listener (removed,
see note above).** This is the
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

## Pure-Rust SRT design proposal (now committed, plan active)

**Update:** this design proposal was moved out of the git-ignored sandbox
path it originally lived in
(`.local/experiments/srt-scaling/rust-srt-design.md`) and into tracked docs:
[`../../srt-pure-rust-design.md`](../../srt-pure-rust-design.md) (the
architecture) and [`../../srt-pure-rust-plan.md`](../../srt-pure-rust-plan.md)
(restream's concrete, phased, gated migration plan built on it, including a
primary-source-verified decision to fork `shiguredo/srt-rs`, and a
Broadcast-first/Backup-optional reprioritization of the bonding gap this
section originally flagged). This section is left as the historical record
of the state before that move; see the two linked docs for current guidance
and execution status.

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

## Harness-native sink extraction and re-verification — 2026-08-15

`RESTREAM_SINK_MODE` was an overloaded name collision with the unrelated,
real, user-facing `sink://` egress output type, and had no production
justification of its own — it existed only so the msr harness could spin up
receiving peers at scale. It has been removed from production `restream`
entirely and replaced with an in-process, harness-native listener: RTMP
reuses the harness's existing `sinks.rs` (`GeneralizedSinkServer`, real
`rml_rtmp` handshake/session negotiation) unmodified; SRT is a new
hand-rolled libsrt FFI module, `src/bin/test_harness/harness_srt_sink.rs`
(`HarnessSrtSink`), following the same "harness owns its own libsrt FFI"
precedent `srt_raw_sink.rs` already established for fault testing.
`grep -rn "sink_mode\|RESTREAM_SINK_MODE\|SINK_MODE" src/` outside
`src/bin/test_harness/` now returns nothing; `sink://`/`EgressProtocol::Sink`
is the only remaining production "sink" concept.

**A real bug was found and fixed during this port**: `harness_srt_sink.rs`
initially declared `SRT_EASYNCRCV = 6003`, which is actually libsrt's
`SRT_ETIMEOUT` value — the real `SRT_EASYNCRCV` is `6002`. Every "no data yet"
response from a non-blocking `srt_recv()` was therefore misclassified as a
fatal error and closed the connection, which closed essentially every SRT
connection immediately after accept (a freshly-accepted socket has no data on
its first poll). Symptom: `canonical`@300 got permanently stuck at 285/300
(the 15 SRT outputs, 5%, endlessly retried). Found by adding `eprintln!`
diagnostics (`tracing::*!` is silently a no-op in `test_harness` — it never
installs a subscriber) and cross-checking the observed error code against
production's own `src/media/srt/sys.rs` constants. Fixed by correcting the
constant and adding `SRT_ETIMEOUT = 6003` alongside it, both treated as
"not a close" — matching production `src/media/srt/ingest.rs`'s own
`SRT_EASYNCRCV | SRT_ETIMEOUT => WaitForReadiness` classification.

Full 12-cell verification matrix, `MSR_PEER=sink`, `PEER_COUNT=1` (the
harness-native sink's parity default, matching the old single-threaded
production `sink_discard_loop` exactly), run against the fixed binary:

| mix | scale | status | elapsed | bytesOutDelta | packetsSentDrop | restreamCpuAvgPct |
|---|---:|---|---:|---:|---:|---:|
| canonical (95/5) | 300 | PASS | 1s | 73,854,675 | 0 | 44.4% |
| canonical (95/5) | 600 | PASS | 1s | 156,679,103 | 0 | 81.4% |
| canonical (95/5) | 900 | PASS | 2s | 247,436,896 | 0 | 103.6% |
| canonical (95/5) | 1200 | PASS | 2s | 324,587,953 | 0 | 156.6% |
| srt-every:2 (50/50) | 300 | PASS | 26s | 80,124,900 | 0 | 120.7% |
| srt-every:2 (50/50) | 600 | PASS | 2s | 154,827,180 | 0 | 96.3% |
| srt-every:2 (50/50) | 900 | PASS | 1s | 265,356,164 | 0 | 183.3% |
| srt-every:2 (50/50) | 1200 | PASS | 21s | 318,468,187 | 353 | 242.9% |
| srt-only | 300 | PASS | 2s | 85,179,980 | 0 | 65.7% |
| srt-only | 600 | PASS | 3s | 170,157,860 | 0 | 144.7% |
| srt-only | 900 | PASS | 22s | 256,831,876 | **570,401** | 178.8% |
| srt-only | 1200 | PASS | **388s** | 347,705,060 | **14,458,511** | 260.3% |

(`rtmp-only` was validated separately at all four scales with zero
`packetsSentDrop` before this fix — it never exercises the SRT sink code
path at all, so it is unaffected by the constant bug and was not re-run.)

Read honestly, not just by the harness's bare `PASS`: `canonical` is clean at
every scale (only 5% SRT, diluted enough to stay clean). `srt-every:2`
(50/50) stays clean through 900 and only shows a small crack at 1,200 (353
dropped packets — real but minor). `srt-only` is clean through 600, then
degrades sharply: 570K dropped packets at 900, and **14.46 million** dropped
packets plus a 388-second convergence time (vs. 1-26s for every other cell)
at 1,200. This is the exact same "harness `PASS` does not mean clean
throughput" pattern documented for the *old* sink-mode implementation in
[Live verification](#live-verification) above, now reproduced in the *new*,
bug-fixed harness-native sink — confirming this is a real, receive-side
degradation intrinsic to a single-listener/single-discard-thread SRT sink at
high real-connection-count, not an artifact of the removed constant bug or
of the old production code path. At `PEER_COUNT=1`, the harness-native sink
is structurally identical to the C-benchmark's own worst case
(`port_count=1`): one listener socket, one accept/read loop.

A follow-up tunable sweep isolating `PEER_COUNT` (port-count-equivalent) and
`HARNESS_SRT_SINK_THREADS` (discard-thread-count) independently at
`srt-only`@1,200 — the worst cell above — is the direct, real-pipeline
answer to the still-open "smallest port_count" question below:

| `PEER_COUNT` | `HARNESS_SRT_SINK_THREADS` | status | elapsed | packetsSentDrop |
|---:|---:|---|---:|---:|
| 1 (baseline, from the matrix above) | 1 | PASS | 388s | 14,458,511 |
| 4 | 1 | PASS | 36s | 2,874,270 |
| 2 | 1 | PASS | 80s | 4,180,495 |
| 1 | 4 | **timed out (900s cap)** | 900s | n/a (never converged) |

Two clear, real-pipeline-confirmed findings:

- **More independent listener instances (`PEER_COUNT`) substantially help**,
  matching the C-benchmark's own `port_count` result qualitatively: going
  from 1 to 4 instances cut convergence time roughly 10x (388s → 36s) and
  dropped packets roughly 5x (14.46M → 2.87M). It does not reach zero drops
  at 1,200 — `port_count=4` is an improvement, not a full fix, in the real
  pipeline just as the isolated C benchmark found. `PEER_COUNT=2` is
  measurably worse than `PEER_COUNT=4` on both axes, consistent with "more
  independent multiplexers is directly better," not a step function.
- **More discard threads sharing one listener (`HARNESS_SRT_SINK_THREADS=4`
  at `PEER_COUNT=1`) is a regression, not an improvement** — the run never
  converged within the 900s progress-timeout cap (worse than the
  single-thread baseline's 388s). Root cause: four OS threads calling
  `srt_accept()`/`srt_recv()` concurrently against the *same* shared
  listener socket/multiplexer, contending on libsrt's own internal locking
  rather than adding capacity. **Since fixed** by rewriting the pool to give
  every thread exclusive port ownership instead of a shared listener — see
  [Exclusive ports-per-thread pool](#exclusive-ports-per-thread-pool--fixes-the-thread-scaling-regression)
  below, which also found that pairing `PEER_COUNT` with a *small* number of
  exclusively-owned threads beats scaling `PEER_COUNT` alone.

## Exclusive ports-per-thread pool — fixes the thread-scaling regression

The `HARNESS_SRT_SINK_THREADS>1` regression above was diagnosed, not left
unfixed: `harness_srt_sink.rs` was rewritten from `HarnessSrtSink`
(one listener, N threads sharing it via concurrent `srt_accept()`) to
`HarnessSrtSinkPool` (N listeners bound up front, `M` threads, each owning a
contiguous, **exclusive** chunk of `N/M` listeners — no two threads ever
touch the same multiplexer). `PEER_COUNT` still governs total port count and
output-URL distribution as before; `HARNESS_SRT_SINK_THREADS` is now a
*total* thread budget for the one shared pool spanning every `PEER_COUNT`
port (default `PEER_COUNT`, i.e. one thread per port, reproducing the old
default exactly), not a per-port count.

Re-running the worst cell (`srt-only`@1,200) against the new pool:

| Ports (`PEER_COUNT`) | Threads (`HARNESS_SRT_SINK_THREADS`) | Ports/thread | Elapsed | `packetsSentDrop` |
|---:|---:|---:|---:|---:|
| 1 | 1 | 1 | 388s | 14,458,511 (old baseline, shared multiplexer) |
| 4 | 1 (old, shared) | 1 | 36s | 2,874,270 (old best, shared-multiplexer design) |
| 4 | 4 (new pool) | 1 | 49s | 3,752,304 (regression check — matches old, within host noise) |
| 8 | 8 (new pool) | 1 | 41s | 3,518,141 |
| 4 | 2 (new pool) | 2 | 18s | 1,515,438 |
| **8** | **4 (new pool)** | **2** | **6s** | **1,144,493** |
| 16 | 8 (new pool) | 2 | 13s | 1,587,509 |

Findings:

- **Exclusive ownership doesn't just avoid the regression — it beats the old
  shared-multiplexer "best" case outright.** `8 ports/4 threads` reaches
  1,144,493 dropped packets, a 92% reduction from the original 14.46M
  baseline and better than the old design's best result (`PEER_COUNT=4`,
  2,874,270) at any thread count tested.
- **2 ports/thread consistently and substantially beats 1 port/thread**, at
  every total port count tried (compare each `ports/thread=1` row against
  the `ports/thread=2` row at the same or nearby port count). One thread
  serving its own listener alone still has to interleave accept and recv
  sequentially with nothing else to do while a connection is idle; two
  exclusively-owned listeners give it more useful work per scheduling
  quantum without introducing any cross-thread sharing.
- **The relationship is not monotonic in port count** — `8 ports/4 threads`
  (6s, 1.14M drops) beats both `4 ports/2 threads` (18s, 1.52M) *and*
  `16 ports/8 threads` (13s, 1.59M), despite the last one preserving the
  same 2-ports/thread ratio at double the scale. Total discard-thread count
  appears to matter independently of exclusivity: this host has 6 cores,
  `restreamCpuAvgPct` already sits at 200-260% (2-2.6 cores) for the sender
  alone, and 8 total discard threads likely start competing for CPU with
  the sender rather than adding capacity. `8 ports/4 threads` is a measured
  local optimum on this host, not a proven global one — a host with more
  cores might push the optimum further out on both axes. Not explored
  further here.

### Exact knee location: 100-step ramp at the pool optimum

The matrix above only checkpoints at 300/600/900/1,200, which pins the
`srt-only` break somewhere in "600-900" — too coarse to call a real number.
Re-run at 100-connection steps, `PEER_COUNT=8`/`HARNESS_SRT_SINK_THREADS=4`
(the local optimum above):

| scale | `packetsSentDrop` | elapsed |
|---:|---:|---:|
| 100 | 0 | 1s |
| 200 | 0 | 1s |
| 300 | 0 | 0s |
| 400 | 0 | 0s |
| 500 | 0 | 2s |
| 600 | 0 | 1s |
| **700** | **35,962** | 2s |
| 800 | 180,264 | 3s |
| 900 | 430,309 | 8s |
| 1,000 | 753,182 | 10s |
| 1,100 | 700,143 | 5s |
| 1,200 | 1,259,544 | 7s |

Zero loss holds flat through 600, then the knee lands exactly at **700** and
rises roughly monotonically after that (1,100 sitting slightly below 1,000
looks like host-timing noise — same order of magnitude, no matching anomaly
in elapsed time — not a real reversal). Comparing this to the same
`PEER_COUNT=8,threads=4` cell's earlier reported 900/1,200 numbers
(430,309/1,259,544 here vs. 430,309/1,144,493 in the ports-per-thread table
above): 900 matches exactly, 1,200 differs by about 10% run-to-run — the
scale of host noise to expect between repeated cells here.

The important negative result: **the exclusive-ownership pool fix improved
severity past the knee dramatically (see the ports-per-thread table above)
but did not move the knee's location** — it's still 700 under the best pool
configuration found, same order of magnitude as the coarse matrix's
"600-900" bound. Whatever breaks at 700 is a different limit than the
thread-contention bug the pool fixed; it's consistent with the genuine
CPU/kernel-socket saturation this investigation's `perf` profiling already
identified (see
[Corrected TCP vs UDP vs SRT ladder](#corrected-tcp-vs-udp-vs-srt-ladder))
rather than anything specific to the harness sink's own threading model.

## Tuple affinity and bonding identity — 2026-08-17

The answer to "can we always route by tuples?" is **no: tuple affinity is the
transport-correctness baseline, not the complete bonding key**.

### Non-bonded SRT

The owner key for a connected UDP socket is the full local/remote UDP 4-tuple
(the harness currently has one public local endpoint, so its map can use the
peer address as a shorthand). Once a worker has connected a datagram socket to
that tuple, all later packets for the tuple must stay with that worker. A
kernel probe with two `SO_REUSEPORT` sockets bound to the same 4-tuple delivered
all 100 test datagrams to one socket (`[0, 100]`), not round-robin. This is why
the connected-handoff design is correct at the UDP ownership boundary, but also
why it cannot split a shared source tuple across workers.

The SRT socket-ID field can distinguish multiple SRT sessions multiplexed by a
single UDP tuple, but it is not a substitute for tuple ownership. On ordinary
data packets the header carries the destination socket ID, identifying the
receiver-side SRT session; the handshake carries the sender's socket ID. Both
are connection-local and must be cached as part of the session state. StreamID
and GROUP are handshake extensions and are not available on every data
datagram.

### Bonded SRT

Bonding adds a second invariant: all physical legs of one logical bond must
land on the same group state machine/worker before steady-state packet
processing. The routing hierarchy is:

1. `GROUP group_id + group_type` from the handshake is the primary bond
   identity. The stock libsrt reference puts group ID, type, flags, and weight
   in `SRT_CMD_GROUP`; the listener creates or finds the peer group by that
   identity and rejects a type collision (`/home/dev/srt/srtcore/core.cpp`,
   `fillHsExtGroup`, `interpretGroup`, and `makeMePeerOf`).
2. Normalized StreamID is the application-level identity/validation key. It is
   useful for proving the legs belong to the same requested stream, but it is
   not a transport ownership key and cannot replace GROUP for a bonded peer.
3. Each leg retains its own full UDP tuple and direction-appropriate SRT socket
   ID for per-connection state, retransmission, sequencing, and teardown.

Do not match bond legs by socket ID alone: each physical leg is a separate SRT
connection and therefore has its own socket IDs. Do not match by StreamID alone:
two unrelated publishers can intentionally use the same StreamID. If GROUP is
absent, malformed, or not allowed by policy, the connection is non-bonded and
must follow the ordinary tuple-affinity path.

This is also why a broadcast receiver should use listener-to-connected handoff
with a **group-owned worker**: both legs reach the same receive merge/dedup
state, avoiding cross-worker sequence arbitration. A least-load policy may
choose the worker when the group is first created, but subsequent legs must
lookup that group and follow it. Disconnect accounting must remove the whole
leg/group membership before a least-load decision is made again.

### What the strategy comparison proves

| Strategy | Correct ownership unit | Observed contention/pathology |
|---|---|---|
| Distinct ports | One libsrt/Rust owner per port | Strong isolation, but extra sockets/threads and a hard host knee remain. |
| One public port + `SO_REUSEPORT` | Kernel hash of the UDP tuple | More tuples distribute well; a shared tuple remains pinned to one worker. Low tuple cardinality produced extreme worker skew in `perf`. |
| Listener-to-connected handoff | Listener assigns one tuple, connected worker owns it | Removes the listener from steady-state traffic, but one shared tuple still serializes all SRT sessions on that owner. Bonding additionally requires group affinity. |

At 600 outputs with 600 independent source tuples, connected handoff balanced
150 tuples per worker but still recorded 34 sender-side drops at 371.0% CPU and
~687 MiB RSS. The matching high-tuple `SO_REUSEPORT` run passed its 600-output
checkpoint but recorded 254,558 drops at 342.7% CPU and ~730 MiB RSS. More
tuples improve ownership distribution; they do not remove SRT protocol cost,
sender backpressure, or the receiver's per-session state cost.

At the full 1,200-output target, the same high-tuple `SO_REUSEPORT` setup did
not reach its first checkpoint within the bounded 180-second run and emitted no
result JSON. The timeout cleaned up the harness, restream, and peer processes.
This is a scale failure, not a missing measurement: tuple cardinality alone
does not make the strategy viable at the target load.

Therefore the current connected handoff is correct for non-bonded tuple
ownership, but it is **not yet bonding-correct**: the harness still needs a
GROUP-aware bond table that assigns every leg to the same worker. A future
connected-owner/per-SRT-session dispatcher may use `(full tuple, destination
socket ID)` after the owner receives the datagram, but it must be measured for
channel and response-path contention before it can be called a performance
improvement.

## Fourth sink topology: one port per stream — 2026-08-17

The Rust sink now exposes a separate `per-stream-port` scaling mode. It binds
one distinct UDP port for every configured `PEER_COUNT` slot, and the MSR
ordinal-to-peer mapping sends each output to its corresponding port. This is
different from the ordinary distinct-port sweep, where a smaller port pool is
shared round-robin by many streams. For a 1,200-output SRT-only run, the
topology is therefore 1,200 Rust sink ports and 1,200 SRT outputs.

The SRT-only path does not bind the harness's unused RTMP listeners. The sink
also rejects a port range that contains restream's own SRT listener, which is
important for the default `8891..10090` range at 1,200 ports. Use a separate
base such as `MTX_SRT=11000` for this run.

The implementation does not claim that a per-stream port removes SRT's
per-session cost. It isolates each stream at the UDP ownership boundary so the
measurement can answer that question directly; CPU, RSS, drops, output
checkpoint, and profile evidence are recorded below after the live run.

### Live evidence

The initial attempt exposed the host prerequisite rather than a protocol
limit: the shell `nofile` soft limit was 1,024, and binding stopped at port
12,015 with `EMFILE`. With `ulimit -n 65535`, the corrected probe counted all
1,200 listeners live on `13000..14199` and passed 30/30 outputs with zero
sender drops. No child process remained after cleanup.

The full run used the bench binary, `PEER_COUNT=1200`,
`HARNESS_SRT_SINK_THREADS=1`, and `MTX_SRT=13000`. It reached 1,200/1,200
outputs and zero sender drops:

| Measure | Result |
|---|---:|
| Rust sink UDP listeners | 1,200 |
| Rust sink workers | 1 |
| Active SRT egresses | 1,200 |
| Restream CPU average / peak | 275.43% / 280.47% |
| Restream RSS peak | 1,276,580 KiB |
| Restream PSS peak | 1,260,201 KiB |
| Harness thread peak | 214 |
| Output checkpoint | 1,200 / 1,200 |
| Sender packet drops | 0 |

Artifacts:

- `.local/artifacts/msr-rust-sink-per-stream-ports-1200-full-20260817/`
- `.local/artifacts/msr-rust-sink-per-stream-ports-1200-probe-counted-20260817/`
- `.local/artifacts/msr-rust-sink-per-stream-ports-1200-profile-20260817/`
- `.local/artifacts/msr-rust-sink-per-stream-ports-1200-sink-worker-profile-explicit-20260817/`

The on-CPU restream profile contained 14K samples with no lost samples. Its
visible cost was dominated by runtime scheduling and waiting around the
libsrt epoll path: `UDT::epoll_wait2` appeared at 2.13% inclusive in the
Tokio scheduler stack. The harness-process sink profile contained 20K samples
with no lost samples. An additional profile attached directly to the one
sink-worker TID contained 3K samples with no lost samples. Its named receiver
hot functions were allocation and receive-buffer work: `_int_malloc` 2.81%,
`ReceiverBuffer::pop_ready` 2.62%, `_int_free` 2.58%,
`SrtConnection::feed_recv_buf` 2.18%,
`process_rust_connections_mode` 1.51%, `ReceiverBuffer::receive` 1.65%, and
`ReceiverBuffer::generate_ack` 1.49% self time. It also showed
`__libc_recvfrom` 0.24% and `__libc_sendto` 0.23%. The unresolved kernel
samples are a host `kptr_restrict` reporting limitation, not evidence of an
application lock convoy.

The controlled conclusion is that one worker is sufficient for this topology
at the current target and that it clears the earlier shared-topology stall.
It does not prove one worker is optimal: all 1,200 sockets are still serviced
by one mio loop, and the profile points to per-packet allocation, receive
buffer processing, ACK generation, and kernel wait time as the next tuning
surfaces. A worker-count sweep is a separate experiment and must keep the
1,200-port map fixed.

The 1,200-port run required raising the process `nofile` limit. Any reusable
benchmark wrapper for this topology must set that limit before binding; the
application now fails early and explicitly if the sink range overlaps
restream's own SRT listener.

## What remains open

- **The smallest `port_count` for a genuinely clean stock-libsrt result at
  1,200 real 8Mbps connections is still not fully resolved, but the
  real-pipeline `PEER_COUNT` sweep in
  [Harness-native sink extraction and re-verification](#harness-native-sink-extraction-and-re-verification--2026-08-15)
  now confirms the qualitative direction**: `PEER_COUNT=4` cuts `srt-only`@1,200
  dropped packets roughly 5x versus `PEER_COUNT=1` (14.46M → 2.87M) and
  convergence time roughly 10x (388s → 36s), but does not reach zero drops —
  4 independent listeners is a real improvement, not a full fix, matching the
  isolated C benchmark's own finding. The 100-step ramp in
  [Exact knee location](#exact-knee-location-100-step-ramp-at-the-pool-optimum)
  pins the actual zero-loss ceiling at **700** connections under the best
  pool config found (`8 ports/4 threads`) — real progress from the coarse
  matrix's "600-900" bound, but the knee itself did not move from tuning the
  harness sink's threading; it is a lower ceiling than "1,200 clean," not
  "1,200 now clean." `test/native/srt-scaling/sweep.sh` is still built,
  checked in, and ready to re-run for a finer-grained answer on the isolated
  C-benchmark side — judge by the `pct_of_target` column, not just error
  counts.
- **`HARNESS_SRT_SINK_THREADS>1` sharing one listener was a real regression
  — now fixed, not just diagnosed.** The original shared-multiplexer design
  (multiple threads calling `srt_accept()` concurrently on one listener) hit
  exactly the class of problem
  [Patched-libsrt exploration](#patched-libsrt-exploration-documented-not-adopted)
  already predicted: a listening port in unpatched libsrt has one shared
  multiplexer, and making it safely parallel across threads needs real
  internal-library surgery the fork itself took two iterations to get right.
  `harness_srt_sink.rs` was rewritten to give every thread **exclusive**
  port ownership instead (`HarnessSrtSinkPool` — see
  [Exclusive ports-per-thread pool](#exclusive-ports-per-thread-pool--fixes-the-thread-scaling-regression)
  above), which not only resolved the regression but beat the old
  single-thread-per-port design outright (92% fewer dropped packets at the
  best-found ratio). The remaining open question is *where the optimum sits
  on hosts with different core counts* — the 8-ports/4-threads local optimum
  found here is specific to a 6-core host and was not chased further.
- **`PEER_COUNT` (independent multiplexers, one per bound port) remains the
  primary scaling axis stock libsrt supports**, matching the C-benchmark's
  own `port_count` result — but per the pool findings above, pairing it with
  a *small*, exclusively-owned thread count per port (2 ports/thread, not 1)
  measurably beats scaling `PEER_COUNT` alone.
- The original in-session claim that `srt-only`@1,200 reported `PASS` 3/3
  times under the *old*, now-removed `--sink-mode` implementation is
  superseded — see the corrected bullet in
  [Live verification](#live-verification) for that history, and the
  harness-native section above for the same finding reproduced (and now
  quantified with a working tunable) in the current implementation. Whether
  the 2026-08-14 netns confound is separately resolved remains open;
  `unshare --net` also remains unavailable in this sandbox.
- **The remaining ~1.7-2.0 Gbps ceiling measured for a single core-pinned
  TX/RX thread pair** (confirmed via `perf` to be genuine per-packet kernel
  cost, not application-level) has not been pushed further with
  `sendmmsg()`/GSO batching or a different I/O model. Future work, not
  started here. **This is about the isolated C benchmark's raw UDP path
  specifically** — a distinct question from `sendmmsg()` for restream's own
  production SRT egress, addressed next.
- **`sendmmsg()`/UDP GSO batching for restream's own production SRT egress
  send path — investigated and closed, do not implement.** A gist
  suggesting this as a fix for restream's `MAX_SRT_MESSAGE_PAYLOAD = 1316`
  fragmentation (a 74KB keyframe becomes ~57 `srt_send()` calls) prompted a
  scoping pass. Findings: 1316 bytes is not a restream tuning choice — it
  sits under libsrt's own live-mode message ceiling
  (`SRT_LIVE_MAX_PLSIZE = 1456`, MTU minus UDP/SRT headers; larger single
  messages are rejected by libsrt with `SRT_ELARGEMSG`). `sendmmsg()`
  itself is inapplicable by construction: restream's Rust code never
  touches a raw UDP socket for SRT — libsrt owns it internally — and the
  vendored libsrt source confirms libsrt itself never calls `sendmmsg()`
  either (its own sender thread uses singular `::sendmsg()`,
  asynchronously, regardless of application-side call count).
  `docs/egress-implementation.md` already investigated this exact CPU gap
  (3.6x vs. legacy) and found the dominant cause was not send-path call
  count at all — batching fragments per scheduler visit only moved the
  number from 158% to 149%, and raising the message ceiling to 1456 was
  reasoned to save only a few percent more. The real cause was SRT egress
  sockets not sharing a UDP multiplexer port (one `RcvQ`/`SndQ` thread pair
  per socket instead of shared); fixing that (muxer-port reuse) closed the
  gap entirely — fabric ended up *below* legacy CPU at scale. Recorded here
  so this doesn't get re-proposed from the gist without this context again.
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
