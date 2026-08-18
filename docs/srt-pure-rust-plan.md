# Pure-Rust SRT: Phased Implementation Plan

**Status: active plan, execution not yet started.** This is restream's
concrete, phased, gated migration plan for replacing the libsrt FFI
dependency with a from-scratch pure-Rust implementation. For the
architecture-level guidance (layering, ratios, invariants) this plan
implements, see [`srt-pure-rust-design.md`](srt-pure-rust-design.md). For
the measurements that motivated this plan, see
[`agent-guidance/quality/srt-scaling-investigation.md`](agent-guidance/quality/srt-scaling-investigation.md).
Track execution progress in
[`agent-guidance/quality/journal.md`](agent-guidance/quality/journal.md) and
[`agent-guidance/quality/baselines.md`](agent-guidance/quality/baselines.md)
as phases land — this document describes the plan, not live status.

restream currently depends on libsrt (C++, statically linked, pinned at
v1.5.5) for all SRT ingest/egress via ~35 hand-written `unsafe extern "C"`
FFI functions in `src/media/srt/sys.rs`. The scaling investigation above
measured a **hard, zero-loss ceiling of 700 concurrent connections** per
best-tuned multiplexer pool on a 6-core host, and found that under identical
thread/socket architecture, raw UDP outperforms SRT by **2.5-4x** — with
`perf` profiling attributing the gap to libsrt's own protocol-layer cost
(ARQ, flow control, TSBPD, pacing), not restream's code or host limits. A
parallel attempt to patch libsrt itself (thread pools + `connect()`-
isolation, `krsna1729/srt` branch `scaling`) fixed real bugs but did not
close the gap and was not adopted.

This prompted the design proposal in `srt-pure-rust-design.md` for a
from-scratch, sans-I/O, layered pure-Rust SRT implementation — Core
(protocol state machine, zero I/O) + Driver (thin I/O shell) — that would
let restream own thread/socket placement instead of inheriting libsrt's
fixed multiplexer model.

Two candidate existing crates were fact-checked: `shiguredo/srt-rs` (active,
Apache-2.0, genuine sans-I/O, confirmed real libsrt/FFmpeg/OBS interop) and
`russelltg/srt-rs` (cleaner protocol/driver split but stale since mid-2024,
self-flagged "NOT PRODUCTION READY"). **Neither supports any SRT group/bonding
type**, which restream uses in production on both ingest and egress — this is
the central open design problem this plan resolves.

**Priority within bonding, set explicitly by direction:** restream already
has a failover switch at the pipeline-input level, so `SRT_GTYPE_BACKUP`'s
failover semantics duplicate a capability restream already owns elsewhere
(confirmed: current production egress bonding, `docs/media-pipeline.md`'s
`## SRT bonding` section, specifically creates `SRT_GTYPE_BACKUP` groups via
the `bond=` URL parameter — this plan deliberately deprioritizes matching
that exact capability). What restream does *not* already have is redundant,
simultaneous-path delivery to beat packet loss on a single link — that is
`SRT_GTYPE_BROADCAST`, and it is the priority bonding target. Backup is
secondary/optional. This reorders and substantially simplifies the bonding
work versus a backup-first plan — confirmed against real libsrt source, see
[Bonding](#bonding-the-central-design-problem).

The goal of this plan: a phased, risk-gated path to replacing libsrt, with
real go/no-go checkpoints (not just a task list), shippable value at multiple
points along the way, and an explicitly acceptable "stop here" outcome if the
premise doesn't hold up under measurement.

---

## Contents

- [Scope and honest framing](#scope-and-honest-framing)
- [Decisions made by this plan](#decisions-made-by-this-plan)
- [Bonding: the central design problem](#bonding-the-central-design-problem)
- [Comparison with rml_rtmp, restream's existing sans-I/O precedent](#comparison-with-rml_rtmp-restreams-existing-sans-io-precedent)
- [Workspace and crate layout](#workspace-and-crate-layout)
- [Integration seams in restream](#integration-seams-in-restream)
- [Coexistence and rollback strategy](#coexistence-and-rollback-strategy)
- [Interop and differential testing against real libsrt](#interop-and-differential-testing-against-real-libsrt)
- [Phases](#phases)
- [Model-tier guidance](#model-tier-guidance)
- [Kill-switch summary](#kill-switch-summary)
- [Critical files for implementation](#critical-files-for-implementation)
- [Verification](#verification)

---

## Scope and honest framing

This is roughly an **8-11 month committed effort** (Phases 0-7, 8a, 9) at
focused single-developer/agent pace, plus an **optional +6-8 weeks** if
Phase 8b (Backup) is ever taken up — it is not part of the committed
timeline (see [Bonding](#bonding-the-central-design-problem)). Phases 3 and 4
(wire format + handshake, then the LIVE data plane) are the bulk of the
committed effort and produce **zero shippable value on their own**. The plan
is sequenced so that value or a kill signal lands at several points before
the end:

| Milestone | Value delivered even if everything after it is abandoned |
|---|---|
| Phase 1 | A committed, source-and-capture-backed spec of libsrt group (Broadcast-first, Backup-secondary) wire behavior — reusable by the patched-fork path too |
| Phase 6 | Rust SRT **egress** shipping behind a flag; if it beats libsrt's drop counts, that alone is a win |
| Phase 7 | Rust SRT **ingest** — this is where the 700-connection ceiling either falls or the premise is disproven |
| Phase 8a | Broadcast-group wire-compat — the actual packet-loss-redundancy value; if it fails, hybrid (libsrt for broadcast-bonded, Rust for everything else) is a **supported permanent end state**, not a failure |
| Phase 8b (optional) | Backup-group wire-compat — lower priority; restream's pipeline-input-level failover switch already covers this use case, so this phase can be skipped or deferred indefinitely without blocking anything else |

The motivating measurements are settled and are not re-derived here: stock
libsrt's zero-loss ceiling of **700 connections** at 8 ports/4 threads on a
6-core host, the **2.5-4x raw-UDP-vs-SRT gap under identical thread/socket
architecture** attributed by `perf` to libsrt's own protocol layer, and the
**patched-fork result** (real bugs fixed, gap not closed, not adopted). See
`agent-guidance/quality/srt-scaling-investigation.md`.

A related, independently useful data point: a standalone Rust prototype
(`test/native/srt-scaling/rs-udp-bench/`) measuring whether `sendmmsg`/
`recvmmsg` syscall batching moves the raw-UDP floor found a real but modest
win (roughly +10-36% throughput depending on thread count, isolated from a
separate, also-real architecture effect of fewer sockets at higher
connection density) — informative for Driver design in Phases 6-7, but not
a stand-in for Phase 4's actual protocol-layer cost comparison. See that
directory's README for the full measured breakdown.

Two dead ends are explicitly out of scope and must not reappear as
mitigations: `sendmmsg()`/GSO batching for restream's production SRT egress
(closed — restream never touches the UDP socket; libsrt only ever calls
singular `::sendmsg()`), and re-attempting the in-libsrt thread-pool patch
from scratch.

Also out of scope because restream does not need them: **File mode,
rendezvous, FEC/packet filters, balancing group type, multicast**. Of the two
group types restream actually uses today, **`SRT_GTYPE_BROADCAST` is
in-scope and prioritized (Phase 8a)**; `SRT_GTYPE_BACKUP` is optional/
deferred (Phase 8b) because restream's pipeline-input-level failover switch
already provides that capability at a different layer.

---

## Decisions made by this plan

| # | Decision | Rationale (short) |
|---|---|---|
| D1 | **Fork-and-extend `shiguredo/srt-rs`**, vendored in-tree, rather than build from scratch | Interop is the dominant risk; shiguredo has *confirmed* libsrt/FFmpeg/OBS interop including a 4-byte control-packet padding shim — an interop detail you only find by painful debugging. Apache-2.0, active, already sans-I/O, LIVE-only matches restream exactly. AGENTS.md: "prefer targeted fixes over rewrites." |
| D2 | **Broadcast group support is the priority bonding target, ahead of Backup** | Restream's actual gap is packet-loss redundancy via simultaneous multi-link delivery, not failover — a pipeline-input-level failover switch already exists. Confirmed against real libsrt source (`group.cpp`) that Broadcast is also the *architecturally simpler* of the two group types (no idle/standby/promotion state machine), so prioritizing it is both the higher-value and lower-risk order. |
| D3 | **Bonding needs Core-level extension points for both group types; it is NOT pure Driver orchestration** | The group ID/type/weight ride in the SRT HSv5 handshake extension, and send-sequence numbering is a group-owned property (`m_iLastSchedSeqNo`, `overrideSndSeqNo`), not private per-connection state — true for Broadcast as much as Backup. See [Bonding](#bonding-the-central-design-problem). |
| D3a | Group send *scheduling* (fan-out to all members vs. active/standby selection) lives in **Core**; group receive *merge* (`CUDTGroup::recv`) is **one shared mechanism for both group types**, also in Core | Confirmed from source: libsrt has separate `sendBroadcast`/`sendBackup` but a single shared `recv`. Backup's extra complexity (stability detection, promotion, parallel-send-during-failover) is entirely send-side and entirely skippable if Phase 8b is deferred — it does not entangle with the receive-merge machinery Phase 8a needs anyway. |
| D4 | Convert repo to a **Cargo workspace with the root package retained**; add `crates/srt-protocol` (Core) and a reusable `crates/srt-lifecycle` seam. Do not create one generic runtime crate that owns a framework's event loop. | Root-package-plus-workspace keeps `src/`, `build.rs`, `benches/`, `tests/` paths and every existing `scripts/build/*` command working unchanged. The lifecycle seam is now justified by the same handshake, GROUP-affinity, alias-routing, connected-handoff, and teardown policy appearing independently in restream ingest and the harness sink. |
| D5 | **Do not pursue `no_std`** | Nice-to-have in the source design doc, hard requirement nowhere. `alloc` is needed for connection setup/teardown anyway. Enforce the *real* invariant (no I/O, no threads, no wall-clock) with a crate-graph architecture test instead. |
| D6 | Keep upstream's internal API shape inside the vendored connection machine; apply the idealized `process_event(&mut self, event, now) -> Outputs` shape **only at the boundaries we own** (group machine, Core↔Driver interface) | Reshaping all of shiguredo's internals to the idealized signature is a rewrite in disguise and destroys rebaseability against upstream fixes. Deliberate deviation from the source design doc. |
| D7 | The **test harness keeps its own libsrt FFI permanently**, and `native-deps.sh` keeps building libsrt even after production stops linking it | The harness's independent libsrt is the interop oracle. Removing production's libsrt makes the harness's *more* valuable, not less. |
| D8 | Hybrid (libsrt for bonded pipelines, Rust for the rest) is an **accepted terminal state** | Removes the all-or-nothing pressure from Phase 8a/8b, the riskiest phases. |
| D9 | Full hot-path determinism (`process_event(event, now)` with `now` always externally injected, never read internally) is a **design goal, not an absolute gate** | restream's own production RTMP path already runs a sans-I/O crate (`rml_rtmp`) that calls `SystemTime::now()` internally rather than taking it as a parameter, and it has been in unincident production use since this codebase's inception. Purity is worth pursuing where cheap, but should not become a blocking perfectionism point the way Phase 3/4's actual interop and cost gates are. See [Comparison with rml_rtmp](#comparison-with-rml_rtmp-restreams-existing-sans-io-precedent). |

### Primary-source verification of `shiguredo/srt-rs` (D1)

D1 was initially decided from secondhand crate research (web search/fetch,
not the actual source). Before Phase 2 commits to vendoring it, the actual
repository was cloned and read directly
(`github.com/shiguredo/srt-rs`, HEAD at clone time `5a8aa3b`, 2026-08-16).
Findings that sharpen, and in one place correct, the earlier research:

- **Correction:** the published crate is `shiguredo_srt` (calendar-versioned,
  `2026.1.0-canary.1` at read time), not `srt-rs` v0.2.3 as earlier secondhand
  research reported. Doesn't affect D4 (Phase 2 vendors via a git checkout at
  a recorded commit, not a crates.io dependency), but the name should be used
  correctly in `VENDOR.md` and any future reference.
- **Core dependency list is exactly two crates**: `aws-lc-rs` (crypto — AES
  cipher primitives, AES key-wrap, PBKDF2-HMAC-SHA1, confirmed via
  `src/crypto.rs`'s own imports) and `tracing` (logging). Crypto is
  genuinely delegated to an audited library, not hand-rolled — stronger than
  the design doc's own budget of "under 10 dependencies" requires.
- **The public API already matches the target Input/Output shape directly**,
  confirmed from `src/lib.rs`'s actual exports: `SrtConnection` driven by
  `ConnectionEvent` → `ConnectionOutput`, with `ConnectionOptions`,
  `ConnectionRole`, `ConnectionState`, `TimerId`. This is the design doc's
  proposed sans-I/O `Input`/`Output` pattern, not an approximation of it.
- **Real per-module test infrastructure already exists**: property-based
  tests for all 9 core modules (`pbt/tests/prop_{buf,connection,crypto,
  handshake,packet,receiver,sender,stream_id,time}.rs`), fuzz targets for
  handshake and packet decode (`fuzz/fuzz_targets/`), 273 `#[test]`
  functions across ~6,400 lines of `src/`, and a vendored copy of the actual
  spec (`refs/srt/draft-sharabayko-srt.md`) that bug-fix commits cite
  directly by section. Phase 3/4 should inherit and extend this
  infrastructure rather than duplicate it.
- **Explicitly confirms the scope exclusion in their own README** ("対象外"
  / "out of scope"): FileCC, Rendezvous handshake, and **Group Membership
  extension** — i.e., bonding is confirmed absent from the primary source
  itself, not inferred from an absence of examples.
- **27 closed + ~20 open tracked issues** (`issues/closed/`,
  `issues/000N-*.md`), many explicitly correctness-focused: AES-CTR IV/
  counter-block construction, sequence-number-wraparound delivery ordering,
  KM refresh counter drift, TSBPD wrapping-period boundary handling,
  listener ISN adoption on handshake. This is disciplined, ongoing
  spec-compliance hardening — a positive signal about process, not evidence
  the implementation is already bug-free.
- **Caution for Phase 2's vendor commit choice:** the unreleased `develop`
  branch's most recent changelog batch (at read time) fixes several
  *fundamental* correctness issues — sequence-wraparound delivery order,
  TSBPD spec-compliance, AES-CTR counter-block construction, and listener
  ISN adoption on `CONCLUSION` receipt. Phase 2 must vendor at a commit that
  includes these fixes (or re-verify them independently if vendoring
  earlier), not assume any given tag or crates.io release has them. This is
  exactly why Phase 3's "100% handshake interop, no partial credit" gate and
  Phase 4's quality-parity gate stay load-bearing regardless of upstream
  pedigree — active maintenance lowers risk, it doesn't retire the need to
  actually verify.

---

## Bonding: the central design problem

**Priority, per explicit direction: Broadcast first, Backup second/optional.**
restream already has a failover switch at the pipeline-input level, so
`SRT_GTYPE_BACKUP`'s value to restream is redundant with a capability that
already exists one layer up. What restream doesn't have today is redundant,
simultaneous-path delivery to beat packet loss on a single stream — that's
`SRT_GTYPE_BROADCAST`, and closing that gap is the actual point of doing
bonding work at all. This section is written broadcast-first; backup is
covered second, as optional/deferred work.

The findings below are grounded in the real libsrt source, not inference —
read directly from a local clone at `/home/dev/srt/srtcore/group.cpp` (a
convenience checkout of `krsna1729/srt`, the patched-fork repo, currently on
`master`) and cross-referenced against upstream `Haivision/srt` at
`/tmp/srt/srtcore/group.cpp`. Neither is pinned to the exact
`v1.5.5`/`b6b4ae99` commit this repo's static build uses — Phase 1 must
re-confirm line references against that exact pinned commit, but the group
mechanism is not expected to have changed materially since.

### Why "N Core connections + a Driver supervisor" does not work, for either group type

The tempting design — N ordinary owned `Connection` values, a Driver-level
supervisor coordinating them, zero Core changes — fails against real libsrt
peers for reasons that hold for **both** group types, confirmed from source:

**1. Group membership is negotiated on the wire, in the handshake.**
libsrt carries a group extension block (`SRT_CMD_GROUP`) inside the HSv5
handshake extension list, containing group ID, **group type** (this is where
Broadcast vs. Backup is actually selected), flags, and link weight. The
listener uses the group ID to decide whether an incoming connection is a
*new* group or a *second member of an existing group* — the exact semantics
restream already documents in `docs/media-pipeline.md` ("duplicate SRT
publishers are not bonded ingest; only libsrt group connections are bonds"),
enforced today by `SRTO_GROUPCONNECT` on the listener
(`src/media/srt/listener.rs:183`, `enable_srt_group_connect`). A Driver
supervisor sitting above unmodified Core connections cannot emit or parse
this extension. **Core wire-format and handshake state machine must change,
regardless of which group type is targeted.**

**2. Send sequence numbers are group-owned, not connection-owned — for
Broadcast too.** This was the biggest open question and is now settled by
reading `sendBroadcast` directly (`group.cpp:1208-1472`): the group tracks a
shared `curseq`/`m_iLastSchedSeqNo`, and every member link's socket has its
send sequence explicitly overridden to match
(`d->ps->core().overrideSndSeqNo(curseq)`, line 1424) before each send. This
is not a Backup-only mechanism — it's how the receiver is able to treat
packets arriving on different physical links as one logical, deduplicable
sequence space at all. **Core connection needs an externally-supplied
send-sequence injection point regardless of which group type Phase 8a/8b
targets.**

**3. The receive side needs a group-level merge — and this machinery is
shared between both group types.** `CUDTGroup::recv` (`group.cpp:2387`) is a
**single, non-duplicated function** — there is no separate
`recvBroadcast`/`recvBackup`. Building the group-level receive merge/dedup
(by sequence number, shared TSBPD basis) once in Phase 8a serves Phase 8b
later at no extra design cost, confirming D3a.

### What Broadcast does NOT need, that Backup does — the actual complexity delta

This is the key finding that changes the plan's risk profile. Reading
`group.h`'s own state-machine doc comment (lines 43-60) and `sendBroadcast`
end to end shows Broadcast is architecturally simpler:

> "Broadcast: links that are freshly connected become PENDING and then IDLE
> only for a short moment to be activated immediately at the nearest sending
> operation." ... "Backup: The link stays idle until it's activated, and the
> activation can only happen at the moment when the currently active link is
> suspected of being likely broken."

Concretely, **Broadcast has no idle/standby holding state, no stability/
failover-detection timer, no promotion logic, and no parallel-send-during-
transition window** — every member link is activated essentially immediately
and sent on every single send call thereafter (`sendBroadcast`'s main loop
iterates all running links and sends the same payload on each). Backup's
extra machinery — RTT-based "suspected broken" detection, held-in-reserve
standby links, promotion-with-sequence-handoff, brief dual-send during
failover — is visible in `group.cpp` as materially more code and more states
than Broadcast requires (explicit code comment at `group.cpp:3488`: *"In
contradiction to broadcast sending, backup sending must check..."*, marking
extra logic that only applies to Backup).

**Net effect on the bounded extension-point list:** Phase 8a (Broadcast) needs
rows 1-3 and 5 below, in a simplified form (no standby run-mode, no
promotion). Phase 8b (Backup, optional/deferred) needs everything, including
the `Idle`/`Standby` mode and promotion logic — the harder half of the
original list.

| Core layer | Needed for Broadcast (Phase 8a) | Additionally needed for Backup (Phase 8b, optional) |
|---|---|---|
| Wire format | Encode/decode the HSv5 group extension block (ID, type, flags, weight) | — (shared) |
| Handshake machine | Emit group extension as caller; parse as listener; surface group identity + type in `HandshakeResult` | — (shared) |
| Connection machine (send) | Accept externally-supplied send sequence + message number, always-active fan-out send | `Idle`/`Standby` run mode (keepalive + ACK/NAK/RTT live, no data scheduling); promotion with sequence continuity; brief parallel-send-during-failover |
| Connection machine (recv) | Expose per-packet sequence + origin timestamp upward; accept externally-imposed TSBPD base | — (shared, same mechanism) |
| Group machine | Member table, fan-out send scheduling, group-level receive merge/dedup under one TSBPD clock | Stability/failover detection (RTT-driven), active-link selection policy, promotion sequencing |
| Application API | `GroupHandle` (Broadcast), status view matching `srt_group_data`/`summarize_group_members` → `src/media/snapshots.rs` | Same `GroupHandle`, extended status fields for member role/state transitions |

**Recommendation, restated:** fork `shiguredo/srt-rs` into
`crates/srt-protocol` and add exactly the Broadcast-column hooks first;
treat the Backup-column additions as a clearly-scoped, separately-gated
follow-on (Phase 8b) that may never get built if the pipeline-level failover
switch continues to cover restream's actual failover need. Read
`russelltg/srt-protocol` as reference (and steal its `srt-c`
differential-testing *pattern*), but do not base on it: stale since
2024-05-31, self-flagged "NOT PRODUCTION READY," TSBPD untested,
differential tests reportedly failing. Do not treat `irlserver/srtla_send` as
an answer — SRTLA is a different protocol layered *on* SRT, not libsrt group
semantics restream must interoperate with.

**This is why Phase 1 is a mandatory, code-free spike, scoped to Broadcast
first.** The table above is now source-grounded rather than a pure
hypothesis, but Phase 1 still needs to nail exact wire-level field layout and
capture real traffic before any Core code is written — and, since Backup is
now optional, Phase 1's packet-capture scenarios should cover Broadcast's
activation/fan-out/merge behavior in depth and Backup's failover only enough
to confirm the extension-point table above, not to fully spec it (that work
is deferred to whenever/if Phase 8b is actually taken up).

---

## Comparison with rml_rtmp, restream's existing sans-I/O precedent

Before treating the Core/Driver split as a novel bet, it's worth naming that
**restream already runs this exact pattern in production, today, on the RTMP
path** — via `rml_rtmp` (crates.io `0.8.0`, pinned in `Cargo.lock`). This is
directly relevant evidence, not a hypothetical analogy, and it should inform
how strictly the SRT plan enforces its own layering rules.

**`rml_rtmp` is genuinely sans-I/O.** Its own `Cargo.toml` depends only on
`byteorder`, `bytes`, `hmac`, `rand`, `rml_amf0`, `sha2`, `thiserror` — no
`tokio`, no `std::net`. Its own doc comment states the intent explicitly:
*"These APIs are networking library agnostic... clients and servers can be
built with `mio`, `tokio`, or even std's networking APIs."* Its core API
shape — `ServerSession::handle_input(&mut self, bytes: &[u8]) ->
Result<Vec<ServerSessionResult>, ...>` — is functionally the same
`process_event(bytes) -> outputs` shape proposed for the SRT Core.

**restream's own code already uses Core/Driver language for this, unprompted.**
`src/media/rtmp/egress_connection.rs:11-15` describes `RtmpSessionCore`
(which wraps `rml_rtmp::sessions::ClientSession`) as: *"Pure,
socket-independent RTMP client session state: owns `ClientSession`... and
produces outbound packet bytes without performing any I/O itself. Driven by
the fabric's non-blocking engine... from a readiness-polled shard visit."*
This is the SRT plan's Core/Driver split, already named and already shipping.

**The Driver precedent to copy for SRT's Phases 6-7 is the egress side, not
ingest.** restream's two RTMP drivers are *not* symmetric, and the asymmetry
is instructive:
- **Egress** (`src/media/egress/backends/rtmp_connection.rs`): plain
  **non-blocking `std::net::TcpStream`** (or TLS via `rustls`), readiness-polled
  by the same shard/poller machinery this plan already targets reusing for
  SRT. This is the closer analog for the SRT Rust Driver — it's the same
  "restream owns thread/socket placement, not the runtime" model this whole
  project is chasing.
- **Ingest** (`src/media/rtmp/ingest.rs`): one **`tokio::net::TcpStream` per
  connection**, spawned as a Tokio task. This works for RTMP because RTMP
  ingest doesn't hit anything like SRT's 700-connection fan-in ceiling — it's
  the wrong model to copy for SRT ingest, where the entire point of Phase 7
  is exclusive port-per-thread ownership, not a task-per-connection model.

**A real, load-bearing precedent for relaxing strict determinism (feeds D9).**
`rml_rtmp`'s `ServerSession`/`ClientSession` call `SystemTime::now()`
internally rather than taking `now` as an explicit parameter — a direct
violation of the idealized sans-I/O determinism invariant. It has not caused
a problem in restream's production RTMP path since this codebase's
inception. This is evidence, not just permission: the SRT Core should still
prefer explicit `now` injection where it's cheap (needed for the simulator-
driven property tests in Phase 4 to be reproducible), but a stray internal
clock read somewhere deep in ported shiguredo code is not automatically a
blocking defect the way a wire-format or interop failure is.

**Where the analogy runs out.** RTMP's own layering (Handshake → `chunk_io`
wire framing → `messages` typing → `sessions` connection state) maps well
onto the Handshake/Connection split proposed for SRT, but RTMP has **no
subsystem-services layer** — no loss list, no RTT estimator, no congestion
window, no TSBPD — because TCP already provides reliability and ordering.
SRT's Phase 4 (the largest phase in this plan) exists precisely *because* SRT
reimplements what TCP gives RTMP for free, over UDP. Don't read "RTMP's crate
is simpler" as "the SRT Core should be simpler too" — the complexity
difference is real and protocol-inherent, not a sign the SRT design is
over-engineered.

**Maintenance-signal note.** `rml_rtmp`'s upstream (`KallDrexx/rust-media-libs`)
has had no commits since 2023-05-31 — dormant, not actively maintained — yet
restream has depended on it since day one without evidence of
production-blocking issues. This tempers, but does not reverse, D1's
preference for `shiguredo/srt-rs`'s active maintenance: active-and-interop-
confirmed is still strictly better than dormant when there's a choice, but
"the upstream might go quiet after we fork it" is evidently a survivable risk
for restream, based on its own experience with `rml_rtmp`.

---

## Workspace and crate layout

### Target layout

```text
Cargo.toml              # [workspace] + [package] restream  (root package retained)
  members = [".", "crates/srt-protocol", "crates/srt-interop"]
src/                    # unchanged; restream package
  media/srt/            # Driver lives here (see below)
crates/
  srt-protocol/         # Core: sans-I/O. Vendored shiguredo fork + group layer.
    VENDOR.md           # upstream commit, fork point, local-patch inventory
  srt-lifecycle/        # reusable sans-I/O admission, affinity, handoff, and
                        # teardown state machine over srt-protocol
  srt-interop/          # dev-only: standalone caller/listener binaries over
                        # srt-protocol + std, for interop tests without linking
                        # restream or libsrt
```

### Why this shape

- **Root package retained.** `[package]` and `[workspace]` can coexist in one
  manifest. Nothing moves; `scripts/build/resource-limit.sh cargo test`,
  `cargo build --profile bench`, `scripts/build/bench-harness.sh`, `build.rs`,
  `benches/`, `tests/` all keep working with no path edits. Moving `restream`
  into `crates/restream` would touch every script, the Dockerfile,
  `scripts/build/app-static.sh`, and the worktree tooling for zero
  architectural benefit.
- **Core as a separate crate is non-negotiable.** It is the only mechanism
  that *structurally* enforces "Core cannot name Driver." A module boundary
  inside `src/media/` would be enforced only by a string-matching
  architecture test (the `tests/media_core_architecture.rs` pattern) — better
  than nothing, but Core's whole value proposition is being fuzzable,
  replayable, and compilable without `tokio`, `std` I/O, FFmpeg, or the
  12-second static-link step. Only a crate boundary buys that.
- **Lifecycle is reusable; event loops are not.** `crates/srt-lifecycle` owns
  pending-handshake state, packet-key aliases, GROUP plus normalized StreamID
  affinity, worker-selection policy, connected handoff, timer/output actions,
  and disconnect/reconnect accounting. It must not depend on Mio, Tokio,
  Glommio, or any other runtime, and it must not create sockets or threads.
  Restream and the harness each keep their own thin socket/event-loop adapter,
  so they retain control of thread and socket placement without reimplementing
  lifecycle policy.
- **No catch-all `srt-driver` crate.** Restream already has the driver-side
  policy: dedicated shard OS threads (`src/media/egress/shard.rs`),
  `SrtFabricPoller` (`src/media/srt/egress_poller.rs`), the ingest accept loop,
  work budgets, and backpressure classification. A framework-neutral driver
  crate would either duplicate that or force restream to adopt someone else's
  threading model. The reusable unit is the lifecycle state machine, not an
  imposed runtime.
- **`crates/srt-interop`** exists so interop tests and fuzzers can run a real
  Rust caller/listener as a subprocess without linking FFmpeg/libsqlite/
  libsrt — seconds vs minutes of build time, materially changing iteration
  speed for Phases 3-5.

### Evidence-led layering revision

The attached scaling analysis and gist are design evidence, not instructions
to copy an architecture wholesale. Their strongest confirmed lesson is the
separation between sans-I/O protocol state and driver-owned socket/thread
placement. The local `/home/dev/srt-rs` checkout reinforces that lesson: its
`SrtConnection` is reusable sans-I/O state and its examples own Tokio sockets,
but its group API alone does not define listener admission, tuple/socket-ID
aliases, worker affinity, connected-socket transfer, or disconnect cleanup.

The current restream tree supplies the missing proof of a second reusable
boundary. `src/media/srt/rust_ingest/connected.rs` and `routing.rs`, and the
harness sink's `connected.rs`, `group.rs`, and `group_runtime.rs`, each carry
versions of the same lifecycle policy. The harness's six framework modules in
`crates/srt-interop` are intentionally benchmark adapters, not a common
lowest-denominator runtime API. Therefore the next extraction is
`srt-lifecycle`, while the six runtime-specific adapters remain independently
owned and benchmarkable.

The dependency direction is intentionally one-way:

```text
srt-protocol  <-  srt-lifecycle  <-  restream/harness lifecycle adapters
                                      ^
                                      +-- mio/tokio/smol/monoio/glommio/compio
                                          remain adapter choices, not core deps
```

`srt-lifecycle` should be extracted only after the current listener-owned
connected handoff is verified. Its first public contract should be the
invariants already required by the live tests: one owner per packet key,
GROUP and normalized StreamID keep all bond legs together, a connected Core
and its timers move together exactly once, and release removes every alias and
group reference. The crate should carry deterministic unit/property tests for
those invariants; socket creation, authorization callbacks, media delivery,
and metrics stay at the application boundary.

### Internal module decomposition of `crates/srt-protocol`

The rml_rtmp comparison answers a question this plan hadn't settled: how far
to split `srt-protocol` internally. **Answer: modules, not further crates —
rml_rtmp itself proves a single well-layered crate is sufficient**, and its
own module boundaries are close enough to a direct template to reuse rather
than invent from scratch:

| `srt-protocol` module | rml_rtmp analog | Notes |
|---|---|---|
| `wire` | `chunk_io` + `messages` | Pure, roundtrip-tested packet/handshake/extension/control encode-decode. RTMP splits this into two layers (chunk framing vs. message typing) because RTMP has two nested framings; SRT has one, so one module suffices. |
| `handshake` | `handshake/` | Direct analog — HSv5 exchange, produces a `HandshakeResult`, then discarded, exactly like rml_rtmp's `Handshake`. |
| `connection` | `sessions/` | Direct analog — the long-lived per-socket state machine. |
| `group` | *(no analog)* | Genuinely SRT-specific: RTMP has no bonding concept. This is real new surface area, not a gap in the RTMP comparison. |
| `subsystems::{loss, rtt, tsbpd, cc}` | *(no analog)* | Also genuinely SRT-specific and the reason Phase 4 is the largest phase: RTMP gets reliability/ordering from TCP for free, SRT reimplements it over UDP. Don't read rml_rtmp's simplicity as a benchmark to match here. |
| `crypto` | *(deliberately absent from rml_rtmp)* | The one place restream's own RTMP and SRT paths diverge structurally, worth naming: RTMP's encryption (RTMPS) is applied entirely in the Driver, wrapping the transport in `rustls` *before* any bytes reach `rml_rtmp` — encryption never enters the sans-I/O crate at all. SRT's encryption is negotiated inside the SRT handshake itself and applied per-UDP-payload, so it **must** live in Core. This isn't a design inconsistency; it's a real difference in where each protocol defines encryption. |

**Feature-gate `group` and `crypto` in `crates/srt-protocol`'s `Cargo.toml`**
(`bonding`, `crypto` Cargo features) even though restream enables both by
default — this keeps the crate's own dependency graph honest per-feature
(matches rml_rtmp's minimalism: `byteorder`/`bytes`/`hmac`/`rand`/`sha2`/
`thiserror`, nothing unused pulled in unconditionally), keeps `wire`/
`handshake`/`connection` independently testable and fuzzable without the
group/crypto surface compiled in, and preserves the option — not a
commitment — of the crate being independently useful/publishable later the
way `rml_rtmp` itself is, without redesigning the module boundaries to get
there.

### Build-tooling interactions (concrete)

- Every heavy command stays prefixed with `scripts/build/resource-limit.sh`;
  `--profile bench`, never `--release`. Workspace-wide `cargo test` now
  includes the new members; use `-p srt-protocol` for the tight Core loop
  (fast inner loop — no native link).
- `scripts/build/bench-harness.sh` remains the **only** path to
  `target/bench/` binaries. Core-only Criterion benches
  (`crates/srt-protocol/benches/`) don't need it; anything the harness
  executes does.
- Touching `build.rs`, `scripts/build/native-deps.sh`, or `test/native/*.c`
  requires `scripts/agent/worktree.sh --no-share-static`. Phases 1, 3, 8, 9
  all touch native inputs — budget the full static rebuild each time.
- `deny.toml`'s `allow-registry = ["https://github.com/rust-lang/crates.io-index"]`
  (confirmed present) means a `git = ` dependency on shiguredo would fail
  `cargo deny`. **Vendoring as an in-tree path member sidesteps this cleanly**
  and is another reason to vendor rather than depend. Record provenance in
  `crates/srt-protocol/VENDOR.md` and add a row to
  `distribution/THIRD_PARTY_COMPONENTS.md` (Apache-2.0 is already in
  `deny.toml`'s allow list — confirmed).
- Secondary compliance win worth noting in the eventual removal phase: libsrt
  is the distribution's only MPL-2.0 component. Dropping it simplifies
  source-distribution/release-compliance obligations.
- `tests/architecture_compliance.rs:108-129` asserts `build.rs` carries
  native-input policy for `libsrt.a`. Do not touch it until Phase 9; it is a
  correct guard until then.

---

## Integration seams in restream

The single most important finding from reading the code (independently
verified, not just claimed): **the egress seam is already backend-neutral.**

`SrtEgressEngine<T>` (`src/media/srt/egress_engine.rs`) implements
`ProtocolEngine` (`src/media/egress/backend.rs:219`) generically over
`T: SrtMessageSender` (`src/media/srt/egress_sender.rs:40`) — confirmed via
direct grep of both files. It contains no libsrt references itself; it does
feed reads, 1316-byte fragmentation, budget accounting, and `WaitCondition`
reporting. The Rust backend therefore does **not** need a new
`ProtocolEngine`; it needs a new `SrtMessageSender` impl and a new poller.
This collapses a large part of what would otherwise be Phase 6.

| Seam | File | What the Rust path supplies |
|---|---|---|
| `ProtocolEngine` | `src/media/egress/backend.rs:219` | **Reused unchanged** via `SrtEgressEngine<T>` |
| `SrtMessageSender` | `src/media/srt/egress_sender.rs:40` | New impl over a Core connection + owned UDP socket; `send_message`/`close`/`native_send_backlog`/`sender_quality_stats` |
| Poller | `SrtFabricPoller`, `src/media/srt.rs:132` | New `SrtRustFabricPoller` with the identical 3-method surface (`register_leaf`/`remove`/`poll_leaves`) over `mio`/epoll on real UDP fds |
| Connect | `connect_fabric_srt_egress_socket`, `src/media/srt/egress_connect/fabric.rs` | Sibling returning a Rust connection handle |
| Shard backend | `src/media/egress/backends/srt.rs` | Generic over the sender/poller pair; `SrtFabricLeaf<T>` already is |
| Ingest | `src/media/srt/listener.rs`, `ingest.rs` | New listener owning N UDP sockets; reuses `SrtIngestPolicyStore`, `srt_stream_id.rs` normalization, `buffer_sizing.rs`, `srt_quality.rs` unchanged |

**Three concrete refactors this seam needs, each small and each landable
early:**

1. **`SrtTraceBStats` leaks the backend into the trait.**
   `SrtMessageSender::sender_quality_stats() -> Option<SrtTraceBStats>`
   (confirmed at `egress_sender.rs:56`) returns libsrt's `#[repr(C)]` struct
   directly. Introduce a neutral `SrtSenderStats` (the ~12 fields
   `srt_sender_quality_from_stats` in `src/media/srt_quality.rs` actually
   reads) and map both backends into it. **Do this in Phase 2**, before
   either backend depends on the other's shape.
2. **`SRTSOCKET = c_int` is used as the leaf handle** in the shard backend
   and poller. Introduce an opaque `SrtLeafHandle` newtype so a Rust
   connection id can occupy the same slot. Also Phase 2 — pure-mechanical,
   independently-testable against the existing libsrt path.
3. **`MAX_SRT_MESSAGE_PAYLOAD = 1316`** (`egress_engine.rs:30`) must stay. It
   sits under libsrt's `SRT_LIVE_MAX_PLSIZE = 1456`; the Rust Core must
   enforce the same live-mode ceiling and reject oversize messages with the
   equivalent of `SRT_ELARGEMSG`.

**Invariants the new code inherits** (AGENTS.md § Media Rules / Hot-Path
Rules): Tokio tasks own sockets and timers; blocking send paths live on
dedicated OS threads; `catch_unwind(AssertUnwindSafe(...))` at every OS-thread
entry point; no failure path may crash the engine; zero per-packet
allocation, logging, locks, async channel sends, or syscalls beyond the
send/recv itself; `Bytes`/`BytesMut` ownership transfer over copies. The Rust
driver has one *new* obligation libsrt hid: it now owns the UDP socket, so
`connect()`-per-peer kernel isolation (validated against Linux v6.8 UDP
socket lookup in the patched-fork work) and exclusive port-per-thread
ownership (the 8 ports/4 threads finding from `HarnessSrtSinkPool`) are
restream's decisions to make and measure.

---

## Coexistence and rollback strategy

Big-bang cutover is unacceptable; the plan uses **three independent levers**,
all in place before any production traffic touches the Rust path.

**Lever 1 — Cargo feature `srt-rust` (compile-time).**
Added to `[features]` in the root `Cargo.toml` alongside the existing
`agent-plane` / `mcp-*` / `egress-test-driver` features (confirmed list).
Off by default until Phase 6 gates pass. With the feature off,
`crates/srt-protocol` still builds and unit-tests (workspace member), but no
production code path references it. Rollback = flip one default.

**Lever 2 — Runtime backend selector (per-process default).**
`RESTREAM_SRT_BACKEND=libsrt|rust` (documented in `docs/configuration.md`),
read once at startup. Default `libsrt` through Phase 8. Rollback = restart
with an env var.

**Lever 3 — Per-pipeline / per-output override (the important one).**
A persisted output-level field resolved through the existing egress
`OutputSpec` / `ProtocolSpec` (`src/media/egress/command.rs`), so a single
problem destination can be moved back to libsrt without touching the rest of
the process. This is what makes the A/B real: both backends live in one
process, feeding the same `TsFeed`, scheduled by the same shard threads,
reporting into the same `snapshots.rs` quality surface — apples-to-apples on
one host under one workload.

**Bonded pipelines are pinned to libsrt** by the resolver until the
corresponding phase passes: Broadcast-bonded pipelines pinned until Phase 8a
passes; Backup-bonded pipelines pinned until Phase 8b passes (which may be
never, per D8/D2 — Backup is optional). That pin makes D8 (permanent hybrid)
a zero-additional-work fallback, and it degrades gracefully: if only 8a ships,
restream still gets non-bonded Rust everywhere plus Broadcast-bonded Rust,
with Backup-bonded pipelines simply staying on libsrt indefinitely.

**Rollback rehearsal is a gate, not a hope:** Phase 6's exit criteria include
a live harness run that flips a running pipeline from Rust to libsrt
mid-stream and back, asserting the operator-visible status contract
(`tests/output_status_contract.rs`, `scripts/check/api-contract.sh`) stays
correct across the flip.

---

## Interop and differential testing against real libsrt

Do not invent a new oracle. The repo already has the right one, in four
tiers.

**Tier 1 — C helpers built against the same static `libsrt.a` restream
links.** `scripts/build/native-deps.sh` already compiles
`test/native/srt-bond-server.c` and `srt-bond-client.c` into
`$PREFIX/bin/restream-srt-bond-{server,client}`, with bonding support in
place. **This is the bonding oracle and it already exists** — Phase 1 needs
to confirm which group type these helpers create by default and add explicit
`SRT_GTYPE_BROADCAST` support if they currently default to (or only support)
Backup. Extend the same loop with per-phase helpers (`srt-interop-caller.c`,
`srt-interop-listener.c`, `srt-interop-lossy.c`) following the identical
build pattern. Subprocess-based: the Rust side never links libsrt, so there
is no FFI in the test path at all.

Note: libsrt is built with `-DENABLE_APPS=OFF -DENABLE_TESTING=OFF`, so
`srt-live-transmit` and libsrt's gtest binaries are **not** available. Do not
flip those on in the pinned build — it changes native inputs, forces
`native-inputs.lock` churn and `--no-share-static` rebuilds for everyone. If
libsrt's own unit-test vectors are wanted for differential testing (the
`russelltg/srt-c` pattern), build them into a **separate opt-in prefix**
behind an env flag, never into the production static prefix.

**Tier 2 — the four-way interop matrix.** Every protocol phase runs all four:

| | Rust listener | libsrt listener |
|---|---|---|
| **Rust caller** | self-consistency | outbound wire compat |
| **libsrt caller** | inbound wire compat | **control** (must also pass, or the harness is lying) |

**Tier 3 — live harness modes, unchanged.**
`mixed.live.srt.h264.a1.bf2` and its 7 siblings (h264/h265 × a1/a2 × bf0/bf2);
`fault.srt-output-stall` (`src/bin/test_harness/fault_recovery/srt_stall.rs`);
msr via `MSR_OUTPUT_COUNTS=1200 MSR_PEER=sink MSR_PROTOCOL_MIX=srt-only
scripts/harness/run.sh msr -- --no-netns`.

**Read msr results correctly.** The investigation doc is explicit that msr's
`PASS` only checks "all outputs present, `bytesOutDelta > 0`" — it passed 3/3
while dropping 10.7-15.5M packets at ~5.6% of target. **Every msr gate in
this plan is stated in terms of `packetsSentDrop`, `bytesOutDelta` vs the
9,600 Mbps target, and `restreamCpuAvgPct` — never in terms of harness
PASS.** The reference ledger to beat: `srt-only`@1200 = 14.46M dropped
(`PEER_COUNT=1`), 2.87M (`PEER_COUNT=4`), 1.14M (post-fix), none of them
clean.

**Tier 4 — the isolated C load generator as the headline oracle.**
`test/native/srt-scaling/sender_bench.c` at 8 Mbps × N connections against
the Rust listener, judged by the `pct_of_target` column, is the direct
apples-to-apples comparison against the measured **700-connection zero-loss
ceiling**. This is the single number the whole project is judged on.
`sweep.sh` is already checked in and ready to re-run. The sibling raw-UDP
tool at `test/native/srt-scaling/rs-udp-bench/` (not SRT-specific — see
[Scope and honest framing](#scope-and-honest-framing)) is a useful reference
for Driver-level syscall-batching design, not a substitute for this tier.

**Core-level testing (no I/O at all).** Because Core is sans-I/O, the
strongest tests are the cheapest: `proptest` (already a dev-dependency) for
wire-format roundtrips, and a deterministic in-process network simulator
(loss/reorder/delay/duplication, virtual clock) driving two Core instances.
Every impairment scenario becomes a fast, reproducible unit test with a
seed — impossible against libsrt today. Target the pyramid: ~50-60% unit
(wire), ~30-35% component (state machine transitions), ~5-10% integration
(real libsrt interop, the wire-regression canary).

---

## Phases

Every phase ends with: a `docs/agent-guidance/quality/` evidence entry, a
`docs/agent-guidance/quality/baselines.md` ledger row where numbers were
produced, and a `journal.md` entry — including for failed or abandoned
phases.

### Phase 0 — Commit the design doc (days)

**Build.** Move the pure-Rust design proposal out of the vanished
`.local/experiments/srt-scaling/rust-srt-design.md` into a tracked path. It
currently exists nowhere in this worktree, so **reconstruct from the pasted
content in the originating planning session before that context is lost** —
the highest-urgency, lowest-cost item in the plan.

Placement, given the existing doc structure:

- `docs/srt-pure-rust-design.md` — the architecture (two-layer split, Core
  layer cake, the source design doc's content, the bonding extension-point
  analysis from this plan).
- `docs/srt-pure-rust-plan.md` — this phased plan.
- Both linked from `docs/README.md` under a plans/decisions/evidence section
  *and* the complete file index (`scripts/check/docs.mjs` fails any doc not
  reachable from `docs/README.md`).
- Both need an H2 `Contents` section listing every H2 (per `docs.mjs`
  convention).
- Add a pointer from `docs/media-pipeline.md`'s SRT section, and **replace**
  the "not committed to the repository" paragraph in
  `docs/agent-guidance/quality/srt-scaling-investigation.md` § "Pure-Rust SRT
  design proposal" with a link — that doc explicitly asks for this.

**Proof.** `node scripts/check/docs.mjs` clean.

**Go/no-go.** Not a technical gate, but a hard prerequisite: no other phase
starts until the design survives outside one sandbox's `.local/`.

### Phase 1 — libsrt group wire spike, Broadcast-first (2-3 weeks) — READ-ONLY, no production code

**Build.** A verified wire-behavior spec, from two independent sources, with
depth weighted toward Broadcast (the priority target) and Backup covered only
enough to confirm/refine the extension-point table (since Phase 8b is
optional and may never be built):

*Source reading* — against the exact pinned commit
(`v1.5.5`/`b6b4ae990daa8193625a4ddeaeaed03023b23125`, re-cloned fresh rather
than reusing the convenience `/home/dev/srt` or `/tmp/srt` checkouts used for
this plan's initial research, which are not pinned to that commit):
`srtcore/group.cpp` / `group.h` — `CUDTGroup::sendBroadcast` (primary focus;
confirmed at `group.cpp:1208-1900` in the unpinned reference checkout),
`CUDTGroup::recv` (shared merge path, confirmed at `group.cpp:2387`),
`sendBackup` and the stability/promotion logic referenced near
`group.cpp:3488` (secondary focus, enough to confirm the Backup-column
extension points, not a full spec), and `srtcore/core.cpp` handshake
extension processing (`SRT_CMD_GROUP` encode/decode, `SRTO_GROUPCONNECT`
listener path — shared by both group types).

*Packet capture* — extend `test/native/srt-bond-server.c` /
`srt-bond-client.c` (already built by `native-deps.sh`; confirm/add explicit
`SRT_GTYPE_BROADCAST` group creation — verify what group type today's
helpers actually default to) plus loopback `tcpdump`, capturing: initial
multi-member Broadcast group handshake, steady-state simultaneous send on all
members, receive-side merge/dedup behavior under injected per-link loss, and
(lighter-weight, Backup only) a forced-failover capture sufficient to
sanity-check the Backup-column extension points.

**Deliverable.**
`docs/agent-guidance/quality/srt-bonding-wire-spec-<date>.md`, answering
concretely: exact HS group-extension layout and field semantics (ID, type,
flags, weight); Broadcast's shared-sequence and fan-out-send mechanism in
full; group-level receive merge and TSBPD basis; what `srt_group_data`'s
`memberstate` transitions correspond to on the wire for Broadcast; and, at
lighter depth, Backup's sequence-sync-at-promotion mechanism, standby
keepalive cadence, and failover trigger condition — enough to validate the
Backup column of the extension-point table without fully speccing
implementation-ready detail (deferred to if/when Phase 8b is taken up). Plus
the **final Core extension-point list** replacing the source-grounded-but-
still-provisional table in [Bonding](#bonding-the-central-design-problem).

**Proof.** Every claim in the spec carries either a `srtcore/*.cpp:line`
reference (against the pinned commit) or a capture artifact. No claim from
memory.

**Go/no-go.**
- **GO** if the required Core changes for Broadcast are a bounded, enumerable
  set of hooks (roughly the Phase 8a column of the extension-point table) —
  this is the gate that actually matters, since Broadcast is the priority
  target.
- **NO-GO / re-plan** if Broadcast group semantics turn out to be pervasive
  across the connection state machine in a way the source reading above
  didn't anticipate — meaning a vendored fork would have to be rewritten
  rather than extended. In that case: adopt permanent hybrid (D8) for
  bonding entirely, or revisit the patched-fork path with the sequence-sync
  knowledge now in hand. (A similar finding limited to Backup specifically
  does not block anything — Backup is already optional per D2/Phase 8b.)

### Phase 2 — Workspace conversion + vendored skeleton + seam de-libsrt-ification (1-2 weeks)

**Build.**
1. Root `Cargo.toml` gains `[workspace]` with
   `members = [".", "crates/srt-protocol", "crates/srt-interop"]`.
2. `crates/srt-protocol` = vendored `shiguredo/srt-rs` at a recorded commit +
   `VENDOR.md` (upstream commit, fork point, running local-patch inventory —
   the rebase contract).
3. `crates/srt-interop` skeleton (empty binaries).
4. The three seam refactors from
   [Integration seams](#integration-seams-in-restream): neutral
   `SrtSenderStats`, opaque `SrtLeafHandle`, `MAX_SRT_MESSAGE_PAYLOAD` ceiling
   documented as a protocol constraint. **All three land against the
   existing libsrt path and are verified by the existing test suite** — no
   new backend involved.
5. `THIRD_PARTY_COMPONENTS.md` row; SBOM regenerated.
6. New architecture test (style of `tests/media_core_architecture.rs`):
   assert `crates/srt-protocol/Cargo.toml` names no `tokio`, no `restream`,
   no socket/thread crate; assert dependency count under the design budget
   of 10.

**Proof.**
- Full `scripts/build/resource-limit.sh cargo test` green, unchanged
  behavior.
- `cargo deny check` clean (validates the vendor-vs-git-dependency
  decision).
- `scripts/build/bench-harness.sh` still populates `target/bench/`.
- `tests/architecture_compliance.rs` untouched and passing.
- Cold and warm build times recorded in `baselines.md` before/after.

**Go/no-go.** GO if the full suite is green and workspace conversion did not
disturb the native static-link step or the worktree `target/` caching model.
NO-GO → revert the workspace and keep Core as an in-tree module guarded by an
architecture test (weaker boundary, project can still proceed).

### Phase 3 — Core: wire format + handshake, LIVE, caller + listener, no crypto (4-6 weeks)

**Build.** Trim the vendored core to restream's scope: LIVE only, caller +
listener only, rendezvous removed, File mode removed, packet filters/FEC
removed. Wire-format layer as pure roundtrip-testable functions. Handshake
state machine producing a `HandshakeResult` struct consumed by the connection
machine — the two never call each other. StreamID handling wired to
restream's existing normalization contract (`src/media/srt_stream_id.rs`).
Reject-reason semantics matching `srt_setrejectreason` /
`srt_getrejectreason` usage. First `crates/srt-interop` binaries.

**Proof.**
- `proptest` roundtrip for every packet type (data + all control types), plus
  a malformed-input corpus that must never panic.
- Component tests for handshake transitions including every rejection path.
- New C helpers `test/native/srt-interop-{caller,listener}.c` built by
  `native-deps.sh` into `$PREFIX/bin`.
- Four-way interop matrix (Tier 2) across: HSv5 with/without StreamID,
  latency negotiation, MSS/FC negotiation, each reject reason restream
  surfaces today.

**Go/no-go.** **100% of the defined handshake matrix passes in both
directions.** No partial credit — everything downstream is built on
handshake compatibility, and a handshake that "usually works" is the worst
possible foundation. If it cannot be made clean here, stop.

### Phase 4 — Core: LIVE data plane (6-8 weeks) — the largest phase, and the decisive one

**Build.** Subsystem services layer: loss list, ACK/ACKACK/NAK generation and
consumption, RTT estimator, TSBPD clock, congestion/flow-control window,
`SRTO_MAXBW`-equivalent pacing, TLPKTDROP / too-late-packet drop policy,
`SRTO_LATENCY` / `RCVLATENCY` / `PEERLATENCY` semantics, `SRTO_LOSSMAXTTL`
reorder tolerance, statistics counters mapping onto the neutral
`SrtSenderStats` from Phase 2. Plus the deterministic in-process network
simulator used by the tests.

**Proof.**
- Component tests per service (loss-list insert/remove/expire; TSBPD
  delivery ordering; window behavior under RTT change).
- Simulator-driven property tests: for any seeded loss/reorder/delay
  profile, delivered stream is byte-identical and in order, or explicitly
  dropped with a counted reason — never silently corrupted, never delivered
  out of order.
- Differential vs libsrt: 10-minute 8 Mbps stream, both directions, at 0.5% /
  1% / 2% injected loss (netem in the harness netns), comparing
  `pktRcvLoss` / `pktRetrans` / `pktRcvDrop` / RTT and the TSBPD
  delivery-time distribution against libsrt↔libsrt on the same impairment.
- **New bench** `crates/srt-protocol/benches/core_packet_loop.rs` (Criterion,
  pure Core, no syscalls): CPU cost per packet, sender and receiver, plain.
  Recorded in `baselines.md`.
- Allocation guard: a test asserting zero allocations in the steady-state
  packet loop (counting allocator), per the three-allocation-points rule.

**Go/no-go — the project's primary technical kill switch.** Both must hold:
1. **Quality parity:** under identical impairment, the Rust receiver
   recovers at least as many packets as libsrt and delivers late/dropped no
   more than 1.2x libsrt's rate.
2. **Cost advantage:** measured per-packet CPU in the Core-only bench is
   **materially below** libsrt's equivalent per-packet cost.

If (2) fails, **stop the project here.** The entire motivation is the
measured 2.5-4x raw-UDP vs SRT gap attributed to libsrt's protocol layer. If
a clean-sheet Rust protocol layer is not cheaper per packet in a pure
micro-benchmark with no I/O in the way, no amount of better thread placement
in Phases 6-7 will recover the ceiling, and continuing would be sunk-cost
reasoning. Failing here is a *successful* outcome of this plan: ~5 months
spent to disprove the premise, with a documented spec and a working Core to
show for it.

### Phase 5 — Core: crypto (2-3 weeks)

**Build.** Validate and adapt shiguredo's existing implementation (AES-CTR,
PBKDF2 key derivation, RFC 3394 SEK wrapping, rekey at 2^25 packets) against
restream's exact usage: `SRTO_PASSPHRASE`, `SRTO_PBKEYLEN` ∈ {16, 24, 32},
`SRTO_ENFORCEDENCRYPTION`. Delegate primitives to audited crates; do not
hand-roll AES.

**Proof.**
- Interop matrix: 3 key lengths × 2 directions × {enforced, not enforced} —
  12 cells, all must pass against the libsrt helpers.
- Wrong-passphrase and passphrase-vs-plaintext mismatches produce the same
  reject reason restream surfaces today.
- Key-rotation crossing 2^25 packets exercised (accelerated via a test-only
  threshold).
- `benches/srt_ingest_latency.rs` already parameterizes
  plain/aes128/aes192/aes256 — run it as the crypto-cost gate and record
  deltas.

**Go/no-go.** All 12 interop cells green; crypto overhead within the same
envelope as libsrt's per the existing bench. Note `russelltg/srt-rs`'s open
key-size-mismatch panic as a specific regression to test for.

### Phase 6 — Driver + production egress, non-bonded, flag-gated (4-6 weeks) — first shippable value

**Build.**
- `src/media/srt/rs_driver/` — owns UDP sockets, epoll/mio registration, the
  timer wheel driving Core's `process_event(.., now)`, and the
  recv→core→output→send loop. Target ratio: if this exceeds ~15% of Core's
  volume, logic leaked downward.
- `SrtRustMessageSender` implementing the existing `SrtMessageSender` trait —
  so **`SrtEgressEngine<T>` and the whole `ProtocolEngine` path are reused
  verbatim.**
- `SrtRustFabricPoller` with the same 3-method surface as `SrtFabricPoller`.
- The three coexistence levers wired end to end (feature `srt-rust`, env
  selector, per-output override), with bonded outputs pinned to libsrt.
- `native_send_backlog()` now reports *our* send buffer — genuinely better
  data than libsrt exposed, feeding the existing backpressure classification
  unchanged.

**Proof.**
- `scripts/build/resource-limit.sh cargo test srt` and the egress fabric
  tests.
- `scripts/check/concurrency/contract.sh` — `srt.rs` lifecycle is explicitly
  in the AGENTS.md gate table; plus `scripts/check/concurrency/fast.sh` for
  the new thread hops.
- `fault.srt-output-stall` passing on the Rust backend, with the same
  operator-visible status contract (`tests/output_status_contract.rs`,
  `scripts/check/api-contract.sh`).
- **Rollback rehearsal**: live flip of a running pipeline
  libsrt→rust→libsrt, status contract correct across both flips.
- All 8 `mixed.live.srt.*` matrix modes on the Rust backend.
- `benches/srt_lifecycle.rs` and `benches/srt_ingest_latency.rs`
  before/after.
- msr `srt-only` at 300 / 700 / 1200 outputs, `MSR_PEER=sink`, judged on
  `packetsSentDrop` and `bytesOutDelta` vs target — **not** on harness PASS.

**Go/no-go.** GO if correctness modes are at full parity **and** msr drop
counts at 1200 are materially better than the libsrt ledger
(14.46M / 2.87M / 1.14M). If correctness is at parity but scale is not
better despite Phase 4's micro-bench win, **stop before ingest** and find out
why — the gap is then in driver design (socket/thread placement), and doing
ingest on a flawed driver design would compound it.

**Shippable here:** even with bonding on libsrt and ingest on libsrt, a
better-scaling egress path behind a per-output flag is real, deployable
value.

### Phase 7 — Driver + production ingest, non-bonded, flag-gated (4-5 weeks) — the headline test

**Build.** Rust listener replacing the `srt_accept` thread model in
`src/media/srt/listener.rs` / `ingest.rs`. This is where restream finally
owns the choices libsrt made for it:
- N independent listener sockets with **exclusive port ownership per
  thread** (the 8 ports/4 threads shape validated by `HarnessSrtSinkPool`),
  not one shared multiplexer.
- **`connect()`-per-peer kernel isolation** (verified against Linux v6.8 UDP
  socket lookup in the patched-fork work) — now trivially available because
  we own the fd.
- No thread pair per multiplexer; a bounded, CPU-derived pool, consistent
  with the egress shard model already in the repo.
- Access control becomes a plain Rust closure instead of the
  `srt_listen_callback` C hook — a genuine simplification, but the StreamID
  **normalization contract must be preserved exactly**
  (`src/media/srt_stream_id.rs`, AGENTS.md § Media Rules).
- Reuse unchanged: `SrtIngestPolicyStore`, `buffer_sizing.rs`,
  `srt_quality.rs`, `srt_monitor.rs`.

**Proof.**
- **The headline measurement:** `test/native/srt-scaling/sender_bench` at
  8 Mbps against the Rust listener, 100-step ramp, judged by `pct_of_target`
  — directly comparable to the measured 700-connection zero-loss ceiling
  under the best stock-libsrt pool config.
- msr `srt-only` @ 700 / 900 / 1200 / 1500, drop counts and `bytesOutDelta`
  vs the 9,600 Mbps target; `restreamCpuAvgPct` recorded (libsrt's
  degradation sat at 230-243% on a 6-core host — well under the ceiling — so
  CPU is a diagnostic, not the pass criterion).
- All `mixed.live.srt.*` modes, ingest side.
- `benches/srt_ingest_latency.rs` before/after;
  `scripts/check/concurrency/contract.sh`.

**Go/no-go — the premise test.** GO if the zero-loss ceiling clears
**≥1400 connections** (2x libsrt's measured 700) on the same 6-core host
under the same `sender_bench` methodology. NO-GO if it does not: keep
Phase 6's egress win if it stands, revert ingest to libsrt, and write up why.
A ceiling that moves from 700 to, say, 850 does not justify owning an SRT
implementation.

### Phase 8a — Bonding: Broadcast group support (4-6 weeks) — the priority bonding phase

**Build.** Implement the Broadcast-column of Phase 1's spec: the group wire
extension in the wire-format layer (shared groundwork, also needed if 8b ever
happens), handshake-side group identity + type in `HandshakeResult`,
connection-side externally-supplied send-sequence injection, and the
`GroupMachine` scoped to `SRT_GTYPE_BROADCAST` — member table, always-active
fan-out send scheduling (no idle/standby/promotion states needed, confirmed
against `sendBroadcast` in Phase 1), and group-level receive merge/dedup
under one shared TSBPD clock. Egress (caller-side group) first, then ingest
(`GROUPCONNECT` listener accept + attach-to-existing-group). Design the
`GroupMachine`'s member-state enum and the wire/handshake layer to have room
for Backup's extra states later, but do not implement them here.

Preserve the documented semantics exactly: *only real group connections are
bonds; duplicate StreamID publishers are not* (`docs/media-pipeline.md`,
AGENTS.md § Media Rules). Group status must populate the same
`snapshots.rs` / `telemetry.rs` fields `summarize_group_members` /
`add_srt_group_quality` fill today.

**Proof.**
- Four-way interop using `$PREFIX/bin/restream-srt-bond-{server,client}`
  (already built; extend for explicit `SRT_GTYPE_BROADCAST` group creation —
  today's helpers default to backup, confirm/adjust in Phase 1): Rust caller
  ↔ libsrt server, libsrt client ↔ Rust listener, Rust ↔ Rust, and libsrt ↔
  libsrt as the control.
- **The actual value proposition, tested directly:** inject asymmetric,
  uncorrelated loss on each member link (netem, independent loss % per link)
  and confirm the merged receive stream has materially lower loss than any
  single link alone — this is what "beat packet loss" means concretely, and
  it should be the headline number for this phase, not just interop pass/fail.
- Simulator-driven property tests on the group machine: for any interleaving
  of per-link loss/reorder/delay, the merged output stream is exactly the
  input stream (deduplicated, in order) whenever at least one member link
  delivers each packet.
- `scripts/check/api-contract.sh` for the bonding status surface;
  `scripts/check/concurrency/fast.sh` for the group machine's thread hops.
- msr and `mixed.live.srt.*` with broadcast-bonded outputs enabled on the
  Rust backend.

**Go/no-go.** GO if all four interop combinations pass and the loss-reduction
test shows a real, measured improvement over single-link delivery. **NO-GO →
adopt D8 for broadcast: broadcast-bonded pipelines stay on libsrt
permanently, everything else runs Rust.** The per-output pin from Phase 6
already implements this.

### Phase 8b — Bonding: Backup group support (6-8 weeks; optional, only if a concrete need surfaces)

**Do not start this phase by default.** restream already has a
pipeline-input-level failover switch; Backup-group support only earns its
cost if a specific need for *socket-level* (not pipeline-level) failover is
identified later — e.g. a customer requirement to interoperate with a
specific third-party encoder that only speaks libsrt Backup bonding. Treat
this as a backlog item gated on that need appearing, not a default part of
the roadmap.

**If undertaken:** implement the Backup-column additions from Phase 1's
spec on top of Phase 8a's group machine — `Idle`/`Standby` connection run
mode, RTT-driven stability/failover detection, promotion with sequence
continuity, brief parallel-send-during-failover. Reuses Phase 8a's wire
format, handshake group-identity plumbing, and receive-merge machinery
unchanged (confirmed shared via D3a).

**Proof (if undertaken).** Failover: kill the active member mid-stream;
assert continuous delivery within the TSBPD latency budget, no duplicate
delivery, no sequence gap, correct `memberstate` transitions. Both
directions, both roles. Otherwise identical proof shape to Phase 8a.

**Go/no-go (if undertaken).** Time-boxed at 8 weeks. GO if all failover
cases pass. NO-GO → adopt D8 for backup specifically: backup-bonded
pipelines stay on libsrt permanently (the pipeline-level failover switch
already covers the operational need this would have served).

### Phase 9 — Default flip, soak, and libsrt removal (3-4 weeks; conditional)

**Build.** Flip `RESTREAM_SRT_BACKEND` default to `rust`; 30-day soak with
the per-output libsrt escape hatch still live. Only then: delete
`src/media/srt/sys.rs` and the FFI-bearing modules, drop libsrt from
`build.rs` / `scripts/build/native-deps.sh` /
`scripts/build/native/native-inputs.lock` /
`distribution/THIRD_PARTY_COMPONENTS.md`, and update the native-input list in
`tests/architecture_compliance.rs:108-129`.

**Explicitly do not remove:**
- The test harness's independent libsrt FFI
  (`src/bin/test_harness/harness_srt_sink.rs`, `srt_raw_sink.rs`,
  `srt_urls.rs`, `core/srt_crypto.rs`) — that separation is by design and is
  now the interop oracle.
- `test/native/srt-*.c` helpers and `test/native/srt-scaling/`.
- libsrt's build in `native-deps.sh` — it must keep producing
  `$PREFIX/lib/libsrt.a` and the `$PREFIX/bin/restream-srt-bond-*` helpers
  for test tooling, even though production no longer links it. Restructure
  it as a test-tooling dependency, not delete it.

**Proof.** Full suite, full mixed matrix, msr at 1200,
`scripts/check/release-evidence.sh`, `scripts/check/source-audit.sh`, SBOM
regeneration, container smoke.

**Go/no-go for removal.** 30 days at default-on with zero SRT-attributed
production incidents and no per-output rollbacks exercised. Otherwise stay
hybrid indefinitely — the flag costs almost nothing to keep and buys a
permanent escape hatch.

---

## Model-tier guidance

Per AGENTS.md § Operational Guidance: *"opus / gpt-5.5: concurrency or
lifecycle redesign, hot-path architecture, benchmark-driven decisions, or
novel protocol behavior."* **This effort is all four simultaneously.**
Treat any temptation to run a phase's *design* on a lower tier as a plan
violation.

**Opus-tier, non-delegable:**
- Phase 1 in its entirety (the spike defines every later API shape).
- The Core↔Driver interface, the group machine design, and every Core
  extension point.
- TSBPD, ARQ, congestion/flow-control semantics.
- Driver thread and socket placement (Phases 6-7) — the actual scaling
  thesis.
- **Every go/no-go decision**, especially Phase 4's kill switch and Phase 7's
  ceiling test.
- Interpreting msr and `sender_bench` numbers (the harness's PASS is known to
  be misleading; reading it correctly requires judgment).

**Sonnet-delegable once the architecture above it is locked:**
- Wire-format roundtrip proptests, after the packet structs are frozen
  (Phase 3).
- New `test/native/srt-interop-*.c` helpers — mechanical, mirroring the
  existing `srt-bond-{server,client}.c` pattern.
- Trait-glue plumbing after the seam is designed: the `SrtSenderStats` /
  `SrtLeafHandle` refactors (Phase 2), backend-selector config plumbing,
  `docs/configuration.md` updates.
- Harness mode registration and manifest edits.
- Doc TOC/index plumbing and `scripts/check/docs.mjs` fixes.
- `baselines.md` / `journal.md` entries from produced numbers.

**Haiku-tier:** repo navigation and retrieval only — locating libsrt source
symbols, listing existing test modes, gathering file inventories.

---

## Kill-switch summary

| Phase | Kill/branch criterion | Outcome if it fires |
|---|---|---|
| 1 | Bonding requires pervasive rather than bounded Core change | Adopt hybrid (D8) up front, or revisit the patched fork with sequence-sync knowledge |
| 2 | Workspace conversion destabilizes native link or worktree caching | Revert; Core as in-tree module + architecture test |
| 3 | Handshake interop not 100% in both directions | **Stop the project** |
| 4 | Rust Core not measurably cheaper per packet than libsrt | **Stop the project** — premise disproven, ~5 months, documented |
| 6 | Correctness parity but no scale improvement at 1200 | Stop before ingest; diagnose driver design |
| 7 | Zero-loss ceiling under 1400 connections | Revert ingest; keep egress if Phase 6 won |
| 8a | Broadcast interop or loss-reduction goal unachieved in 6 weeks | **Permanent hybrid for broadcast** — libsrt for broadcast-bonded, Rust for the rest (already implemented via the per-output pin) |
| 8b | Not started by default; if undertaken, backup interop unachieved in 8 weeks | **Permanent hybrid for backup** — libsrt for backup-bonded pipelines (already the case if 8b is simply never started; the pipeline-input failover switch covers the operational need) |
| 9 | Any SRT incident during the 30-day soak | Stay hybrid indefinitely; keep the flag |

---

## Critical files for implementation

- `src/media/srt/sys.rs` — the complete FFI surface being replaced (~35
  functions, ~15 socket options, the bonding structs
  `SrtGroupMemberConfig`/`SrtSocketGroupData`); the definitive scope
  checklist for Core+Driver feature parity.
- `src/media/egress/backend.rs:219` — the `ProtocolEngine` trait the Rust
  backend plugs into; reused unchanged (verified).
- `src/media/srt/egress_engine.rs` — proof the egress seam is already
  libsrt-free (`SrtEgressEngine<T>` generic over `T: SrtMessageSender`,
  verified via grep); the reason Phase 6 needs a new sender + poller, not a
  new engine.
- `src/media/srt/egress_sender.rs:40` — `SrtMessageSender` trait, the actual
  swap point (verified); also where `sender_quality_stats() ->
  Option<SrtTraceBStats>` leaks libsrt's struct into the trait and must be
  neutralized in Phase 2.
- `scripts/build/native-deps.sh` — the pinned libsrt build
  (`ENABLE_BONDING=ON`, `ENABLE_APPS=OFF`, `ENABLE_TESTING=OFF`) and the
  existing `test/native/srt-bond-{server,client}.c` helper build loop that
  becomes the interop-oracle extension point.
- `src/media/srt/listener.rs:183` — `enable_srt_group_connect` /
  `SRTO_GROUPCONNECT` bonded-ingest enablement (verified) that Phase 7
  replaces and Phase 8 must reimplement wire-compatibly.
- `docs/agent-guidance/quality/srt-scaling-investigation.md` — the full
  measurement record this plan is built on; read before starting any phase.
- `deny.toml` — confirmed `allow-registry` restricted to crates.io and
  Apache-2.0 already allow-listed; governs the vendoring decision (D1, D4).
- `srtcore/group.cpp` (`sendBroadcast` at ~line 1208-1900, `recv` at ~line
  2387, `sendBackup`/stability logic near ~line 3488), `srtcore/group.h`
  (state-machine doc comment ~line 43-60) — the source grounding for the
  entire [Bonding](#bonding-the-central-design-problem) section. Available
  locally at `/home/dev/srt` (patched fork, `krsna1729/srt`) and `/tmp/srt`
  (upstream `Haivision/srt`) as convenience checkouts, **neither pinned to
  the exact `v1.5.5`/`b6b4ae99` build commit** — Phase 1 must re-clone at
  that exact commit before citing final line numbers in the wire spec.
- `src/media/rtmp/egress_connection.rs:11-15`, `src/media/egress/backends/rtmp_connection.rs`,
  `src/media/rtmp/ingest.rs` — the existing sans-I/O Core/Driver precedent
  this plan's architecture is validated against; see
  [Comparison with rml_rtmp](#comparison-with-rml_rtmp-restreams-existing-sans-io-precedent).
- `test/native/srt-scaling/rs-udp-bench/` — the raw-UDP Rust prototype and
  its measured `sendmmsg`/`recvmmsg` batching results; informs Phase 6-7
  Driver design, not a substitute for Phase 4's protocol-layer comparison.

---

## Verification

- Phase 0: `node scripts/check/docs.mjs` clean; both new docs reachable from
  `docs/README.md`.
- Phase 2: `scripts/build/resource-limit.sh cargo test` (workspace-wide)
  green; `cargo deny check` clean; `scripts/build/bench-harness.sh` still
  populates `target/bench/`; `tests/architecture_compliance.rs` passing
  unchanged.
- Phases 3-5: Core-only test/bench loop via `cargo test -p srt-protocol` and
  `cargo bench -p srt-protocol` (no native link needed) plus the four-way
  interop matrix against the libsrt helpers built by `native-deps.sh`.
- Phases 6-8: full live-harness verification per phase as specified above —
  `mixed.live.srt.*` modes, `fault.srt-output-stall`, msr scale runs read by
  `packetsSentDrop`/`bytesOutDelta` (never bare PASS), and rollback
  rehearsal via `tests/output_status_contract.rs` /
  `scripts/check/api-contract.sh`.
- Phase 9: `scripts/check/release-evidence.sh`, `scripts/check/source-audit.sh`,
  SBOM regeneration, container smoke, full mixed matrix, msr at 1200.
- Every phase: a `docs/agent-guidance/quality/` evidence doc, a
  `baselines.md` row for any numbers produced, and a `journal.md` entry —
  even for phases that fail their go/no-go and stop the effort there.
