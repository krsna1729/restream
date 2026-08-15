# SRT Fan-In Scaling: First-Principles libsrt Investigation — 2026-08-15

## Contents

- [Summary](#summary)
- [Background](#background)
- [Method: isolated libsrt benchmarking, real 8Mbps](#method-isolated-libsrt-benchmarking-real-8mbps)
- [Results](#results)
- [TCP control comparison](#tcp-control-comparison)
- [What restream already had right](#what-restream-already-had-right)
- [What was folded back into restream](#what-was-folded-back-into-restream)
- [Patched-libsrt exploration (documented, not adopted)](#patched-libsrt-exploration-documented-not-adopted)
- [Pure-Rust SRT design proposal (research artifact, not adopted)](#pure-rust-srt-design-proposal-research-artifact-not-adopted)
- [Live verification](#live-verification)
- [What remains open](#what-remains-open)

## Summary

A gist cataloguing suspected libsrt scaling weaknesses prompted a first-principles
re-investigation of whether restream's SRT egress problems at 1,200 concurrent
outputs (see
[`srt-egress-scale-investigation-2026-08-10.md`](srt-egress-scale-investigation-2026-08-10.md))
were symptoms of a deeper libsrt architecture limit. Building isolated C
benchmarks against stock (unpatched) libsrt — bypassing restream, mediamtx, and
Tokio entirely — confirmed the gist's core claim: **a single shared libsrt
multiplexer (one UDP socket, one receive-queue thread) degrades under real,
sustained 8Mbps-per-connection load well before 1,200 connections**, independent
of any restream code. The smallest number of independent listener ports that
gives a clean (zero error, zero failed-connection) result at 1,200 real
connections is **4** — confirmed with 5 repetitions per cell.

A separate patched-libsrt line of work (bounded thread pools + `connect()`-based
per-peer kernel isolation, pushed to a public fork) explored fixing this inside
libsrt itself. It resolved two real concurrency bugs along the way but did not
close the 900–1,200-connection gap, and per explicit redirect was deprioritized
in favor of the simpler, already-available fix: more independent ports on stock
libsrt. That fork and an accompanying pure-Rust SRT design proposal remain as
reference material, not production changes — see the two sections below.

Live-verified against the actual msr harness: `canonical`@1,200 passes
cleanly as before, and `srt-only`@1,200 — the exact mix the 2026-08-14
netns-confound investigation found reliably failing under this sandbox's
`--no-netns` fallback — passed cleanly 3/3 times in this session (see
[Live verification](#live-verification)), a genuinely new result at the same
N≥3 bar this investigation used for the C-benchmark sweep above.

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

All of this work lives in `.local/experiments/srt-scaling/` (git-ignored,
outside the repository), built against the same static `libsrt.a` restream
links. Key tool: `c/minport-sweep.sh`, driving `c/sink_bench.c` (stock libsrt,
`SRTO_UDP_RCVBUF` = 192MB "tuned" value, clamped by `net.core.rmem_max`) and
`c/sender_bench.c` (stock libsrt, `SRTO_MAXBW` = 1,000,000 B/s = exactly
8Mbps per connection) through a 600/900/1,200-connection checkpoint ramp.

- `port_count` ∈ {2, 4, 8}: number of independent listener ports/multiplexers
  the sink opens; senders spread evenly across them.
- 5 repetitions per cell, host-load-gated (`/proc/loadavg` ceiling + a
  `pgrep`-based liveness check between cells, so no cell starts on a host still
  cooling down from the previous one).
- Bitrate: `BITRATE=1000000` bytes/sec = 8,000,000 bits/sec = 8Mbps exactly,
  matching restream's real 1080p60 MSR fixture, not a lighter placeholder rate.

## Results

| `port_count` | total `steady_send_errors` (5 reps) | total failed connections (5 reps) |
|---|---|---|
| 2 | 9,334 | 232 |
| **4** | **0** | **0** |
| 8 | 0 | 0 |

`port_count=4` is the smallest configuration that gives a perfectly clean
result at 1,200 real 8Mbps connections; `port_count=2` is confirmed unreliable
at real scale. `16` was never needed — `8` already came back clean, so the
sweep stopped at the first two clean tiers per the original instruction
("smallest port count to give 0 error 1200 pure SRT result").

This directly corroborates the gist's central claim: stock libsrt's
single-multiplexer receive path (one UDP socket, one `CRcvQueue` worker thread,
shared across every connection accepted on that port) is a real, reproducible
scaling ceiling well under 1,200 connections, entirely independent of restream,
mediamtx, or Tokio.

## TCP control comparison

`c/tcp_sink.c`/`c/tcp_sender.c` ran the identical 600/900/1,200 ramp over plain
TCP instead of SRT, as a structural control: does TCP need the same
port-multiplication workaround?

**No.** TCP handled the full ramp to 1,200 connections cleanly on a **single**
port with 6 worker threads — 0 failed connections, 0 send errors, sub-5ms p99
connect latency. The reason is structural, not protocol-specific: every
`accept()` on a TCP listener hands back a dedicated kernel socket with its own
buffers, automatically, at the kernel level. libsrt's default architecture has
no equivalent — every connection accepted on one SRT listener shares that
listener's one `CRcvQueue`/`CSndQueue` and one underlying UDP socket unless the
application opens more listener ports itself.

Caveat for anyone reusing `tcp_sender.c`'s throughput numbers: its pacer
computes the per-connection send interval correctly but applies it as the
interval for one *round* across all connections owned by a thread, not per
connection, so aggregate `steady_bytes_sent` plateaus around 440-480MB/s
regardless of population instead of scaling with connection count. The
zero-error/zero-failed connection-scaling result is unaffected by this; the
raw throughput figures should not be read as a true 8Mbps-per-connection
aggregate baseline without fixing that pacer first.

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
latency override immediately after. This is a straight buffer-size increase on
a receive-dominant socket — the same knob validated end-to-end in the isolated
libsrt sweep above — applied to bring the test harness's own receiver up to
the same standard restream already holds its production listener to.

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
pushing. **Not adopted into restream** — per explicit redirect, the
stock-libsrt multi-port fix above is simpler, already available, and
sufficient for restream's actual need (four independent listener ports, not a
custom libsrt fork). The fork remains as reference material for anyone
revisiting libsrt's internals later.

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
  scripts/harness/run.sh msr -- --no-netns`, **3 repetitions**, host-load
  settled below 4.0 (1-min `/proc/loadavg`) before each: **PASS, 3/3, zero
  errors/warnings/retries in every run** — `outputs=1200/1200` in 274s, 374s,
  and 303s respectively (rep 1 showed a brief mid-ramp dip, 847→830 around
  11-51s, before recovering and climbing steadily to 1,200; reps 2-3 climbed
  more directly). `unshare --net` remains unavailable in this sandbox
  (`Operation not permitted`, unchanged from the 2026-08-14 confound), so
  this still isn't proof the confound is *resolved* — but the 2026-08-14 doc
  found `srt-only`@1,200 failing identically across every code variant under
  `--no-netns`, and this is 3/3 clean passes of that exact mix in that exact
  environment, at the same statistical bar (N≥3) this investigation held
  itself to for the C-benchmark sweep above.

## What remains open

- **`srt-only` at 1,200 passed cleanly under `--no-netns` 3/3 times in this
  session** (see above) where the 2026-08-14 investigation found it reliably
  failing across every code variant tried. `unshare --net` itself is still
  unavailable in this sandbox, though, so this remains `--no-netns` evidence
  only — re-run under real network-namespace isolation once available, to
  rule out something specific to this sandbox's shared-namespace behavior
  rather than the sink-mode buffer fix generalizing cleanly.
- **The 900–1,200-connection degradation is unresolved in the patched-libsrt
  line of work** and was not chased further once the simpler stock-libsrt
  multi-port fix proved sufficient. If a future need requires *fewer than 4*
  independent ports at 1,200 real connections, the patched-libsrt fork and its
  thread-confined pool design are the starting point, not a fresh rewrite.
- **`tcp_sender.c`'s pacer bug** (round-scoped instead of per-connection
  rate enforcement) means its raw throughput numbers are not a valid
  machine-sizing baseline yet — only its connection-scaling result is.
  Unfixed; flagged for whoever next needs an accurate TCP throughput
  comparison.
- **`rust-srt-design.md` is not in version control.** If the pure-Rust SRT
  direction gets picked up, move it into `docs/` first so it survives outside
  this one sandbox's `.local/` state.
