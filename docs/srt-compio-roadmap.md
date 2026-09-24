# SRT / Compio Dataplane Roadmap

Status: active  
Scope: `restream` + upstream `srt-rs`  
Primary branch: `restream/redevelop`

Current known heads at the time this tracker was established:

- Restream: `c7c9b3c123ceb335a036143d80709a3ea51bcbe9`
- srt-rs: `7382e26f80410bef43da149a3550978bcc7a4d49`

This document is the canonical execution roadmap for the Restream SRT dataplane
migration, performance work, packet-I/O substrate decisions, and the subsequent
RTMP/kTLS dataplane work.

It is intentionally different from:

- `docs/agent-guidance/quality/backlog.md`
  - owns independently schedulable quality/performance debt
- `docs/agent-guidance/quality/baselines.md`
  - owns durable performance evidence
- `docs/agent-guidance/quality/journal.md`
  - owns dated execution history
- `docs/egress-architecture.md`
  - owns current architecture
- `docs/media-pipeline.md`
  - owns current media-flow semantics
- `docs/layering-roadmap.md`
  - owns repository/package layering

This roadmap owns:

- execution order
- architectural invariants
- hard-cut decisions
- performance targets
- benchmark gates
- decision points between normal socket I/O and AF_XDP
- the relationship between SRT, RTMP, io_uring, and kTLS

## Contents

- [SRT / Compio Dataplane Roadmap](#srt--compio-dataplane-roadmap)
- [1. End-State Objective](#1-end-state-objective)
- [2. Core Architectural Invariants](#2-core-architectural-invariants)
- [3. Completed Work](#3-completed-work)
- [4. Work Item 2 — SRT Egress Compio Cutover](#4-work-item-2--srt-egress-compio-cutover)
- [5. Current Work — WI3.1](#5-current-work--wi31)
- [6. WI3.2 — Restream SRT Ingress Hard Cut](#6-wi32--restream-srt-ingress-hard-cut)
- [7. WI3.3 — Delete Old SRT Transport Machinery](#7-wi33--delete-old-srt-transport-machinery)
- [8. Performance Program After SRT Ingress Cutover](#8-performance-program-after-srt-ingress-cutover)
- [9. Fixed Performance Targets](#9-fixed-performance-targets)
- [10. WI3.4 — Establish the Packet-Rate Benchmark Contract](#10-wi34--establish-the-packet-rate-benchmark-contract)
- [11. WI3.5 — Packet-I/O Substrate Shootout](#11-wi35--packet-io-substrate-shootout)
- [11.1 WI3.5B — Modern CPU cross-host validation (deferred)](#111-wi35b--modern-cpu-cross-host-validation-deferred-to-wi10)
- [12. What io_uring Does and Does Not Give Us](#12-what-io_uring-does-and-does-not-give-us)
- [13. WI3.6 — SRT Packet Engine: Incremental Cost Above the Substrate](#13-wi36--srt-packet-engine-incremental-cost-above-the-substrate)
- [14. Same-Peer GSO vs Cross-Destination Batching](#14-same-peer-gso-vs-cross-destination-batching)
- [15. Zero Copy](#15-zero-copy)
- [16. WI3.7 — End-to-End Multi-Shard 8 Mbps Capacity Qualification](#16-wi37--end-to-end-multi-shard-8-mbps-capacity-qualification)
- [17. Re-Derive the SRT Shard Law](#17-re-derive-the-srt-shard-law)
- [18. WI3.8 — AF_XDP Decision Gate](#18-wi38--af_xdp-decision-gate)
- [19. AF_XDP Scope](#19-af_xdp-scope)
- [20. AF_XDP Must Not Become a Universal Networking Abstraction](#20-af_xdp-must-not-become-a-universal-networking-abstraction)
- [21. RTMP / RTMPS Networking Strategy](#21-rtmp--rtmps-networking-strategy)
- [22. kTLS and AF_XDP](#22-ktls-and-af_xdp)
- [23. Why Keep RTMP on Kernel TCP](#23-why-keep-rtmp-on-kernel-tcp)
- [24. RTMP / RTMPS Transport Convergence](#24-rtmp--rtmps-transport-convergence)
- [25. WI4A — SRT Productionization](#25-wi4a--srt-productionization)
- [26. WI7 — Dataplane Shrink](#26-wi7--dataplane-shrink)
- [27. WI8 — CPU / NUMA / Capacity Oracle](#27-wi8--cpu--numa--capacity-oracle)
- [28. WI9 — Abstraction Compression](#28-wi9--abstraction-compression)
- [29. WI10 — Final Qualification](#29-wi10--final-qualification)
- [30. Performance Diagnosis Tree](#30-performance-diagnosis-tree)
- [31. Benchmark Discipline](#31-benchmark-discipline)
- [32. Result Storage](#32-result-storage)
- [33. Existing Backlog Mapping](#33-existing-backlog-mapping)
- [34. Work-Item Tracking Convention](#34-work-item-tracking-convention)
- [35. Current Sequence](#35-current-sequence)
- [36. Immediate Next Action](#36-immediate-next-action)
- [37. Definition of Success](#37-definition-of-success)

## 1. End-State Objective

Restream should have one production SRT implementation and one coherent dataplane
ownership model.

Target SRT shape:

```text
control plane
    Tokio
        |
        | bounded commands / snapshots
        v

fixed dataplane shard threads
    |
    +-- one Compio runtime per SRT shard
    |
    +-- at most one IPv4 srt_transport::compio::Owner
    |
    +-- at most one IPv6 srt_transport::compio::Owner
    |
    +-- bounded application leaves / cursors
```

Target RTMP shape:

```text
control plane
    Tokio
        |
        v

fixed dataplane shard threads
    |
    +-- Compio TCP
    |
    +-- normal kernel TCP
    |
    +-- kTLS for RTMPS where qualified
```

There must not be permanent parallel production implementations such as:

```text
TokioSrtBackend
CompioSrtBackend
NativeSrtBackend
AfXdpSrtBackend
SRT_RUNTIME=...
```

The target is one protocol implementation with the smallest practical packet-I/O
substrate underneath it.

## 2. Core Architectural Invariants

These are not optimization suggestions. They are constraints.

### 2.1 Ownership

- fixed shard ownership
- no OS thread per connection
- no task per SRT caller
- no UDP socket per SRT caller
- no runtime per output
- one Compio runtime per SRT shard
- at most one SRT Owner per address family per shard

### 2.2 Media

- bounded shared feeds
- cursor-based consumption
- no per-output media queue
- slow output cannot pin shared retention indefinitely
- immutable/shared media where possible

### 2.3 Scheduling

- bounded work budgets
- explicit ready/deadline scheduling
- no population scans in hot scheduler paths
- no service-to-quiescence loops
- Owner service once per ready batch, not per leaf
- exact identity for completions/events
- stale generation safety

### 2.4 Protocol

- preserve SRT wire behavior
- preserve protocol deadlines separately from application scheduling deadlines
- preserve stream-id admission policy
- preserve encryption semantics
- preserve bonding semantics
- preserve exact peer/group identity

### 2.5 Runtime

- io_uring production path is fail-closed
- no hidden fallback to Tokio or Poll
- RawReadiness is a receive mode inside a working Compio/io_uring runtime
- RawReadiness is not an alternative to io_uring
- ManagedPreferred stays the production policy unless evidence proves otherwise

### 2.6 Cleanup

When a replacement path is accepted:

- delete the old path in the same architectural tranche
- do not preserve compatibility for its own sake
- do not maintain dual runtimes
- preserve behavioral contracts, not old implementation structure

## 3. Completed Work

### WI1 — Dependency and thread-ownership pivot

Status: DONE

Restream:

`ec8832d98058d35a34d6c7fe57bfc9d572e08105`

Key outcomes:

- upstream `srt-rs` adopted
- vendored copy removed
- `srt-transport` uses Compio feature
- backend creation moved onto shard thread
- backend no longer required to be `Send`
- native io_uring SRT path marked transitional

### WI1.5 — Runtime-aware idle wait

Status: DONE

Restream:

`e5da0de38e7e65413dfa64a87723dfd9c6004def`

Key outcomes:

- bounded Flume command channel
- backend-aware `wait_idle`
- wake reasons separated
- future runtime can wait on commands + transport activity

### WI1.6 — Drain-deadline integration

Status: DONE

Restream:

`c323e5f5e832a618f71edd873ddcd3dc6c1bde30`

Key outcome:

- idle waits bounded by application drain deadlines

### WI1.7 — Upstream bonded Owner API

Status: DONE

srt-rs PR #121

Key outcome:

- `Owner::connect_bonded`
- one logical caller
- bounded physical legs
- transactional admission

## 4. Work Item 2 — SRT Egress Compio Cutover

Status: DONE IN FULL

### WI2 — Initial Compio Owner egress hard cut

Restream:

`25e58506d0146655c4d34bf94f543c5bf9a0e2f4`

Key architecture:

```text
SRT shard thread
    |
    +-- one Compio runtime
    |
    +-- optional IPv4 Owner
    |
    +-- optional IPv6 Owner
    |
    +-- application leaves
```

Important bug discovered during qualification:

Compio `block_on()` can return with a ready future without necessarily polling
the io_uring driver.

A shard that never parked therefore stopped reaping TX completions.

Required embedding contract:

```rust
runtime.poll_with(Some(Duration::ZERO));
runtime.run();
owner.service(...);
```

once per ready batch.

Regression test:

`a_shard_that_never_parks_still_reaps_tx_completions`

### WI2.1 — Correct SRT bonding identity

Status: DONE

srt-rs PR #123

Final dependency ancestry includes:

`d3f31d16e631edc1c1bbcef6b55070e659822188`

Established libsrt-compatible identity rule:

```text
caller group identity != receiver group identity
```

For a bonded caller:

- first successful responder GROUP ID pins remote receiving group
- later legs must report same receiving-group ID
- address equality is irrelevant
- independent receivers are not one bond
- unreachable leg is ordinary degradation
- conflicting receiving group is a logical bond failure

### WI2.2 — Exact CallerPool maintenance and request-local deadlines

Status: DONE

srt-rs final merged SHA:

`7382e26f80410bef43da149a3550978bcc7a4d49`

Key outcomes:

- pool owns capacity
- request owns connect deadline
- queue wait does not consume request deadline
- exact resolved-attempt index
- exact due-deadline index
- bounded maintenance
- maintenance and TX have independent budgets
- listener/caller maintenance fairness
- listener idle maintenance included in pending-work calculation

### WI2.3 — Restream repin and semantic integration

Status: DONE

Restream:

`0642245721f0380585b7cafff841ec4f4e1ca95c`

Key outcomes:

- Restream pinned to `7382e26f...`
- Owner settings became capacity-only
- old `max_actions >= capacity * 2` workaround removed
- request-local `connect_timeout` restored
- bonded peer-group collision is fatal to the logical output
- exact caller attribution retained
- stale event safety retained

### WI2.4 — io_uring deployment and RawReadiness qualification

Status: DONE

Restream progression:

- `01ac1136...`
- `c7435cbc...`
- `ca26097d...`
- final passing head `f2c3deda50f73dacb71dc62daa0d99a61d19ac8a`

Key outcomes:

#### Docker

Production Docker seccomp profile is shipped under:

`distribution/docker/restream-seccomp.json`

It is based on Moby Docker `v29.8.1` default policy plus:

```text
io_uring_setup
io_uring_enter
io_uring_register
```

No:

- `--privileged`
- broad capabilities
- normal `seccomp=unconfined`

Real Docker proof:

- Docker Engine 28.0.4
- kernel 6.17.0-1022-azure
- production image
- USER 1000:1000
- default Docker profile blocks io_uring startup
- shipped profile succeeds
- real harness-driven SRT egress succeeds

#### Managed RX

Three distinct states are recognized:

```text
A. io_uring unavailable
   -> runtime cannot exist
   -> fail closed

B. io_uring works
   provided-buffer/multishot unavailable
   -> RawReadiness

C. io_uring works
   managed RX substrate available
   -> ManagedMultishot
```

RawReadiness host:

```text
BufferRingRegistrationFailed(22)
rx_mode = RawReadiness
```

Managed-capable Docker host:

```text
managed_rx = Available
rx_mode = ManagedMultishot
```

### WI2.5 — Final egress qualification

Status: DONE

Qualification branch progression ended at:

`c7c9b3c123ceb335a036143d80709a3ea51bcbe9`

#### Healthy fanout

Candidate:

```text
10 outputs    3/3 healthy
30 outputs    3/3 healthy
100 outputs   3/3 healthy
300 outputs   1/1 healthy
500 outputs   1/1 healthy
```

Candidate medians:

```text
10 outputs:
    CPU ~125%
    RSS ~137 MB

30 outputs:
    CPU ~144%
    RSS ~139 MB

100 outputs:
    CPU ~173%
    RSS ~146 MB

300 outputs:
    CPU ~259%

500 outputs:
    CPU ~330%
```

Old baseline `c323e5f5...` collapsed under the same workload.

#### Owner behavior

At all tested candidate fanouts:

- TX high-water reached 16
- TX exhaustions remained zero
- TX submissions matched completions
- no stuck-full TX pool
- no non-idle completion starvation
- no RX truncation
- no RX ring drops

#### CallerPool

Live bounded queue proof:

```text
capacity = 16
peak queue = 6
queue drain ~3.3 ms
100/100 outputs progressed
```

#### Exact same-Owner slow-peer proof

Final amendment:

`c7c9b3c123ceb335a036143d80709a3ea51bcbe9`

Topology:

```text
live SRT shards = 2
target shard = 1
1 slow output + 10 healthy outputs
all IPv4
all deterministically assigned to shard 1
only shard1/v4 Owner active
```

90-second application-pause result:

```text
10/10 healthy siblings progressed continuously
slow receiver application delivered 0 events while paused
Owner remained unfaulted
TX submissions == completions
TX exhaustions = 0
feed retention plateaued around 9.4 MB
```

This closes the shared-Owner slow-peer isolation proof.

#### Known non-blocking debt

Q-026:

Frozen-destination RSS threshold currently fails on both old and new
implementations, but both plateau.

This is not a Compio regression.

Track separately in:

`docs/agent-guidance/quality/backlog.md`

## 5. Current Work — WI3.1

### WI3.1 — Upstream production listener admission for Compio Owner

Status: DONE

srt-rs PR #125, squash-merged as `86369b0815c09333547c965652250e17f580e834`
(content head `08eb56b46d999be71c90666e2da850210b44b778`).

Key architectural results:

- `ListenerAdmissionResolver` (Arc-backed, one per listener) and
  `Owner::listen_with_resolver` on the Compio, Mio and Tokio Owners;
  `Owner::listen` is unchanged.
- Compio RawReadiness and ManagedMultishot share one admission helper.
- A listener advertises GROUP only to callers that sent a GROUP request; the
  response id is the application-owned receiving-group id, distinct from the
  caller's group id.
- Known non-blocking note: Compio `Owner::listen_inner` assigns
  `socket_memory_budget` before RX-mode resolution/bind, so "transactional"
  means the same semantics as the pre-existing `listen()`.

Repository:

`srt-rs`

Base:

`7382e26f80410bef43da149a3550978bcc7a4d49`

Goal:

Make Compio Owner listener production-complete before Restream ingress moves to
it.

Required upstream capabilities:

#### A. Listener admission resolver

Owner must support the existing:

```text
AdmissionRequest
    ->
AdmissionResolution
```

policy path.

This is required to preserve Restream's per-StreamID:

- authorization
- latency selection
- passphrase
- encryption key length
- rejection
- bounded defer

The resolver must be:

- synchronous
- bounded
- cache/in-memory only
- called only during admission
- not called for ordinary DATA/ACK/NAK traffic

#### B. Receiving-group identity

Bonded listener responses must use an application-owned receiving-group ID.

Do not confuse:

```text
caller GROUP ID
```

with:

```text
receiver GROUP ID
```

One logical receiver may reuse one receiving-group identity across multiple
endpoints.

Independent receivers must have distinct receiving-group identities.

This is required for true libsrt-compatible bond validation.

#### Runtime parity

Resolver semantics should exist for:

- Compio Owner
- Mio Owner
- Tokio Owner

RawReadiness and ManagedMultishot must use one common listener-admission helper.

Restream must not be changed until this lands upstream.

## 6. WI3.2 — Restream SRT Ingress Hard Cut

Status: DONE

Started after WI3.1 merged.

Result (Restream pins srt-rs `86369b0815c09333547c965652250e17f580e834`):

- `native_ingress.rs`, `native_ingress_drive.rs` and their tests are deleted; SRT
  ingress has no `UringUdpDriver`, `CompatReceiver` or shadow `PeerTable`.
- One owner thread (`srt-in-<port>`) builds one production Compio runtime and
  one `Owner` attached with `Owner::listen_with_resolver`; Restream owns the
  receiving-group identity (`ingress_admission.rs`).
- Tokio addresses sessions only by `LogicalPeerId` through bounded commands
  (`Send`, `Disconnect`, `Shutdown`; capacity 256, 32 per owner visit) and
  events (capacity 256); a full event bridge backpressures, never drops
  accepted media.
- Async authorization rejection and pipeline deletion disconnect the real Owner
  peer; SRT read/play sends through the Owner logical peer.
- Final Restream SHA entering cleanup: `b1e2d2d42545189403a3a1b69d842a00f482418a`.
- Evidence: hosted ingress live tests pass (including the read/play proof, after
  it was decomposed onto a fixture publisher); the real `srt.policy` scenario
  passes plaintext, AES-128, AES-192 and AES-256 publish/read plus the plaintext
  and wrong-passphrase rejections. The qualification host selected: driver
  `IoUring`, `io_uring=true`, `managed_rx=BufferRingRegistrationFailed(22)`
  (`buffer-ring-unsupported-by-kernel`), `rx_mode=RawReadiness`,
  `rx_policy=ManagedPreferred`. ManagedMultishot ingress was exercised only by
  hosted CI, not by the local process-level run.
- Unrelated/pre-existing quality debt (not WI3.2 failures): the RTMP
  `fault.resilience` sink-disappear/churn/stall failures (present at pre-cut
  `2b328f13`) and the nondeterministic transcoder
  `prebuffered_h264_packets_drive_internal_scaled_stage` unit test.

Replace:

```text
NativeSrtIngress
UringUdpDriver
Restream-owned PeerTable driving
```

with:

```text
one ingress shard thread
    |
    +-- one Compio runtime
    |
    +-- one srt_transport::compio::Owner listener
```

Preserve:

- owner-thread ingress ownership
- StreamID policy
- passphrase resolution
- bonded ingress
- LogicalPeerId
- bounded app events
- protocol deadlines
- lifecycle semantics
- selected-input semantics
- MPEG-TS demux
- generation safety

Do not:

- add a Tokio fallback
- retain native SRT ingress in parallel
- add a runtime-selection flag

## 7. WI3.3 — Delete Old SRT Transport Machinery

Status: DONE

After ingress passes qualification, remove all obsolete Restream SRT transport
machinery.

Result: `crates/restream-dataplane/src/udp.rs` and `udp_recv.rs` are deleted along
with the UDP-only `OpKind` variants (`UdpRx`, `UdpTx`, `UdpRecvMulti`) and the
UDP allocation-test section; at that point, crate docs described its surviving
role as native TCP/io_uring for RTMP, reusable scheduler/media primitives, and
synthetic harness. WI5 now moves production RTMP to Compio TCP.
The dead zero-match SRT concurrency steps are removed, the never-consumed
`RESTREAM_SRT_UDP_BUFFER` / `srt_udp_buffer` setting and the undocumented-in-code
`RESTREAM_SRT_IO_BATCH_CAPACITY` docs are gone, and
`production_srt_does_not_own_native_udp_transport` guards production SRT source.

Amendments (Restream `bdbf001e`, `808347d2`, `d56406f2`, and the final
correction below):

- `bdbf001e` removed the dead listener telemetry (`udpRxQueueBytes`,
  `udpRxQueuePeakBytes`, `udpDrops`, the `udp_drops` alert, the fabricated
  "SRT Listener Socket" diagnostic) and the libsrt-era `AGENTS.md` rules.
- `808347d2` wired the per-publisher receive quality that the receive-buffer
  alert and Publisher Transport diagnostic read: the ingress Owner samples
  `srt-rs` receiver statistics and Tokio folds them into the ingest snapshot;
  occupancy is authoritative packet capacity, and the guessed byte fields are gone.
- `d56406f2` replaced the shared sample map with a stamped, bounded,
  LOSSY Owner-to-Tokio telemetry bridge (a full bridge drops and counts the
  sample, never delaying protocol service; rates use Owner-to-Owner intervals,
  duplicates are ignored, counter resets give no rate). Bonded peers report
  bond identity and member state, a LOGICAL payload rate, and explicit wire
  loss/undecryptable fields instead of summed leg counters. The egress
  `mbps_send_rate` unit bug (MB/s reported as Mbps) is fixed, and the stale
  libsrt wording in the frontend and docs is corrected.
- The final correction (`d3d92bae`) makes egress quality stateful at the ~1 Hz
  stall sweep: `mbps_send_rate` is the interval delta of the caller's own wire
  sender bytes (`total_srt_bytes_sent` direct, `wire_srt_bytes_sent` bonded),
  never the peer's advertised receive rate, and a first sample, a counter reset
  or an absent sender direction reads `null` instead of a fabricated zero —
  RTT and bonded drop counters likewise stay `null` until the transport
  reports them. Bonded ingress instantaneous RTT/jitter/latency/buffer
  occupancy comes only from connected legs, and `srtGroupConnectedMembers`
  counts unstable legs as connected because upstream defines them as connected
  links excluded from delivery by backpressure. The Publisher Transport
  diagnostic and the dashboard show the bonded wire degradation counters
  instead of unavailable ordinary counters, and the SRT Listener Owner check
  counts SRT ingests only and reports `telemetryDropped`.
- Known non-blocking debt: `ingressOwner.managedRx` is a bool, so it cannot
  distinguish "no Owner selected a mode yet" from RawReadiness.


Delete as applicable:

- Restream native SRT UDP io_uring driver
- duplicate ingress PeerTable ownership
- SRT-specific fixed-file/SQE/CQE machinery now superseded by Compio
- stale compatibility wrappers
- stale naming
- obsolete runtime abstraction layers
- dead tests that only preserve deleted transport paths

Keep:

- media rings
- schedulers
- WorkBudget
- generation safety
- capacity/oracle metrics
- protocol correctness tests
- fault tests
- slow-peer tests
- useful generic dataplane primitives

## 8. Performance Program After SRT Ingress Cutover

This tranche begins only after there is one canonical SRT datapath in both
directions.

The purpose is to answer:

```text
What prevents >=1 Mpps/core?
```

rather than assuming Compio, io_uring, the Linux UDP stack, or SRT is the
limiting factor.

## 9. Fixed Performance Targets

These targets are deliberately recorded here so they do not drift after
measurements are obtained.

### 9.1 Product workload

Primary product workload:

```text
1 x 1080p30 encoded input
8 Mbps media bitrate
no transcoding
SRT ingress
SRT egress fanout
```

At approximately 1316-byte SRT media payload:

```text
~760 DATA packets/s/destination
```

At 1000 destinations:

```text
~760,000 DATA packets/s
~8 Gbit/s media payload
~8.5 Gbit/s or more on wire before control/retransmission headroom
```

Therefore the 1000-destination performance environment should use:

```text
>=25 GbE
```

A 10 GbE link is too close to physical saturation to be a meaningful CPU
qualification environment.

### 9.2 Normal socket / io_uring substrate floor

Synthetic packet-I/O target:

```text
>=2 M UDP datagrams/s/core
```

This remains the target, but its attainability is host-dependent: the measured
development host reaches ~0.3 Mpps/core on a TX-only lane with the stock IPv4/UDP
transmit path (`§11`), so the target is an absolute capacity claim that is only
settled by the cross-host comparison in `§11.1`. It is not a precondition for
WI3.6.

Shape:

- ~1316-byte payload
- one pinned CPU
- one shared unconnected UDP socket
- ~1000 IPv4 destinations
- preconstructed packets
- no SRT
- no media ring
- no crypto
- no scheduler beyond packet submission

### 9.3 SRT engine target

Healthy no-loss target:

```text
>=1 M SRT DATA packets/s/core
```

Secondary target:

```text
~1.25-1.5 M total SRT packet events/s/core
```

where total events include:

- DATA
- ACK
- ACKACK
- NAK
- keepalive
- retransmit
- timer-driven protocol work

### 9.4 Product stretch goal

```text
1 x 8 Mbps 1080p30 SRT ingress
    ->
1000 x 8 Mbps SRT outputs

<= 1 aggregate datapath CPU core
```

Conditions:

- no transcoding
- healthy no-loss network
- control-plane CPU reported separately
- >=25 GbE
- explicit RX mode reported
- no output stalls
- bounded feed retention
- no transport fault

This is a stretch goal, not something to assume current code already satisfies.

## 10. WI3.4 — Establish the Packet-Rate Benchmark Contract

Status: WI3.4A DONE (contract frozen; local reference artifact recorded — tree
`a4024c64`, ledger commit `f72387ed`, artifacts under
`.local/artifacts/wi3-lane-50/` and `.local/artifacts/wi3-lane-100/`);
WI3.4B PENDING INFRASTRUCTURE (external-host baseline)

WI3.4 splits into two deliverables with different prerequisites:

```text
WI3.4A  measurement contract
        DONE once frozen and exercised by a reproducible local reference
        artifact (single-host development lane). Blocks WI3.5.

WI3.4B  external-host reference baseline
        PENDING INFRASTRUCTURE: a real second host (>=10 GbE for 100 outputs;
        >=25 GbE for the 1000-output qualification). Does NOT block WI3.5/WI3.6.

WI3.7-final
        requires real multi-host / physical-NIC qualification.
```

The contract is mature enough to measure and reject a contaminated run, which
was its purpose; waiting for a second machine to record a `healthy` remote rung
would gate the packet-engine work on infrastructure it does not need.

Build durable benchmark/evidence machinery for:

```text
100 outputs
300 outputs
500 outputs
1000 outputs
```

using the 8 Mbps workload.

Measure:

- DATA pps
- protocol-control pps
- CPU
- cycles/packet where possible
- RSS
- TX submissions/s
- TX completions/s
- SQEs/submission
- io_uring enters/s
- service visits/s
- Owner actions/s
- scheduler ready depth
- scheduler wake rate
- retransmissions
- kernel drops
- NIC drops
- shard load

This card establishes the measurement contract.

It does not optimize yet.

### 10.1 Scenario ladder

One 1080p30 H.264 SRT ingest at 8 Mbps
(`test/fixtures/transport/bench-h264-8m.ts`) fanning out to N SRT outputs
against harness-native `srt-rs` sink peers, one isolated stack per rung. The
fixture's effective rate is part of the contract: `tests/fixtures.rs`
asserts that its bytes over the demuxed media span are 8.0 Mbps ±0.4, so an
encoder or generator drift fails in CI instead of silently redefining the
workload (the label is the payload rate, not the `-b:v` video target).

```text
N = 100, 300, 500, 1000
```

The rung is driven by the existing resource-sweep mode (`MSR_PEER=sink` swaps
MediaMTX for in-process sink listeners so the peer side is not the bottleneck):

```sh
MSR_PEER=sink \
RESOURCE_SWEEP_BITRATE=8M \
RESOURCE_SWEEP_EGRESS_COUNTS=<N> \
RESOURCE_SWEEP_SCENARIOS=egress-growth-source-srt \
RESOURCE_SWEEP_SAMPLE_SECS=10 \
RESOURCE_SWEEP_SETTLE_SECS=10 \
WORK_DIR=.local/artifacts/wi34-ladder/<N> \
scripts/harness/run.sh resource-sweep
```

Run one rung per invocation (`RESOURCE_SWEEP_EGRESS_COUNTS` with a single
value): a run that walks several counts grows the fan-out cumulatively, so
its later rungs are not independent rungs.

For the 300/500/1000 rungs the peers belong on other machines — the loopback
sink pool saturates long before 1000 × 8 Mbps. Start a sink peer on each
remote host and point the rung at them:

```sh
# on each peer host
SRT_SINK_PORTS=8891 scripts/harness/run.sh srt-sink -- --no-netns

# on the measuring host — pin BOTH peer ports, or the harness synthesizes its
# own per-process ports and the outputs never reach the peer
MSR_PEER=sink \
RESOURCE_SWEEP_SRT_PEER_HOSTS=peer-a,peer-b \
MTX_SRT=8891 MTX_API=9997 \
RESOURCE_SWEEP_BITRATE=8M \
RESOURCE_SWEEP_EGRESS_COUNTS=1000 \
RESOURCE_SWEEP_SCENARIOS=egress-growth-source-srt \
WORK_DIR=.local/artifacts/wi34-ladder/1000 \
scripts/harness/run.sh resource-sweep -- --no-netns
```

`MTX_SRT` must match the peer's `SRT_SINK_PORTS` and `MTX_API` its
`SRT_SINK_STATE_PORT`: the harness otherwise allocates per-process ports, so a
run without them points at ports nothing is listening on (observed as
`handshake attempt deadline` on every output).

SRT outputs are spread over the configured hosts by a stable hash of the
output name, and the rung records how many outputs each peer is expected to
receive so delivery can be checked against the workload.

Artifacts per rung, all under `WORK_DIR`:

| File | Contents |
|---|---|
| `resource-sweep-results.json` / `.csv` | CPU, RSS, memory attribution, ring/AVIO occupancy per rung |
| `resource-sweep-samples.jsonl` | The same, one line per sample |
| `packet-contract.json` | Run metadata (git SHA/dirty, workload, peers, windows), one summary per `(scenario, output count)` rung, the validity verdict with reasons, and the `unavailable` block |
| `packet-contract-samples.jsonl` | One contract record per sample |

### 10.1a Single-host development lane (veth + CPU partitioning)

Namespace-separated sinks over a veth pair plus explicit CPU partitioning make
the development lane materially better than loopback without pretending to be
a second machine: separate network stacks, socket tables, routing and qdisc
paths, and a receiver that cannot steal the measured core.

```text
                 host
                   |
        +----------+-----------+
        |                      |
   Restream cpuset          sink cpuset
   (RESTREAM_CPUSET)        (SRT_SINK_CPUSET)
        |                      |
      veth0 ---------------- veth1
        |                      |
   root netns              sink netns
```

`scripts/harness/veth-topology.sh up` creates the netns/veth pair and writes
`local://wi3-topology.env` (peer host, ports, topology kind and both CPU
masks) for the run; `down` removes it. The measured datapath is pinned with
`RESTREAM_CPUSET` (the harness spawns restream under `taskset`) and the sink
with `SRT_SINK_CPUSET` (applied before the acceptor threads start, so they
inherit it). `packet-contract.json` records the topology and the *observed*
masks — restream's `Cpus_allowed_list` from `/proc/<pid>/status` and each
peer's mask from its `/state` endpoint — not just what was requested.

A same-host rung is baseline-eligible only when the masks are disjoint:
partitioning is what stops the receiver from consuming the measured CPUs.

Measured on the reference 6-vCPU host: with restream on CPUs 0-2 and the sink on
3-5, 50 × 8 Mbps delivers 98.9 % of the workload to the peer, while 100 ×
8 Mbps is receiver-limited (57 %). The receiver is therefore the limiting side
somewhere between those rungs on this host — a host property, not a workload or
link property, and the reason WI3.5 uses cheap UDP drains rather than SRT
receivers.

What this lane can establish: SRT protocol cost, Restream scheduler cost,
syscall/io_uring cost, UDP/IP stack cost, socket-queue pressure, batching,
wakeups, copies, crypto, cross-thread scheduling and CPU scaling. What it
cannot: PCIe/NIC DMA, hardware queues, IRQ/NAPI placement, offloads,
physical-link drops, real 10/25/100 GbE behaviour, AF_XDP zero-copy driver
performance or NUMA locality. Those stay in the multi-host final qualification.

### 10.2 Metric contract

Each contract record is sourced from production surfaces only
(`/metrics/system`: `egressShards`, `capacity`, `ioUring`) plus host counters,
so the numbers describe the shipped datapath rather than a benchmark-only
build. Cumulative counters are differenced between samples; a first sample, a
counter reset, or a missing source reads `null`, never a fabricated zero.

| Metric | Source | Kind |
|---|---|---|
| TX datagrams/s | Σ `egressShards[].srtOwners[].txPackets` delta — every datagram handed to a socket, DATA plus control. This is not "total SRT events": RX and timer/maintenance work are separate rows below | rate |
| DATA pps | Σ `srtOwners[].txClass.dataFirst` delta — first transmissions only. This is the number to compare against the ~760 DATA packets/s per destination of the product workload, and against §9.3's ≥1 M DATA pps/core | rate |
| retransmissions/s | Σ `srtOwners[].txClass.dataRetransmit` delta | rate |
| protocol-control pps | Σ `srtOwners[].txPackets` − (DATA + retransmissions) delta | rate |
| TX completions/s | Σ `srtOwners[].txCompletedOk` delta | rate |
| RX datagrams/s | Σ `srtOwners[].rxPackets` delta | rate |
| service visits/s | Σ `srtOwners[].serviceVisits` delta | rate |
| Owner actions/s | Σ `srtOwners[].serviceActions` delta, plus `maintenanceActions` | rate |
| scheduler wake rate | not sourced yet: `ShardMetrics::record_useful_wake`/`record_empty_wake` have no production caller, so `feedWakesUseful`/`feedWakesEmpty` read 0 for every backend | gap |
| loop iterations/s, media ticks/s, ready visits/s | Σ `loopIterations`, `mediaTicks`, `readyVisits` delta — the scheduler-activity signals that are actually produced | rate |
| scheduler ready depth | `readyDepth` / `readyDepthHwm`, max over shards | gauge |
| fault/pressure counts | rated-window deltas of `ownerTxFailedSends`, `ownerTxExhaustions`, `ownerServiceBudgetExhausted`, `ownerRxRingDropped`, `ownerRxTruncated`, `shardFeedResyncs`, `shardDriverBudgetViolations`, `shardQueueOverflows`, reported under the summary's `countsPerWindow` (they are interval counts, not rates). Lifetime totals stay as `*Total` for context; the verdict judges only the deltas, so ramp-up or settle-period events cannot condemn a steady-state window | count |
| workload delivery | the peer's own `payloadBytesDelta` integrated over **that peer's observed intervals** (`observedSecs`), compared against `expectedOutputs × 1,000,000 B/s × observedSecs` within the fixture's ±5%. Peer polling can drift from the host's sampling cadence, so `rate × host interval` is never used; the rung records `peerDelivery` (`deliveredBytes`, `observedSecs`, `coverageRatio`, delivered/expected B/s) and requires the peer to have observed at least the ten-second minimum. The fixture is VBR and that ±5% is a whole-span average, so per-second rates stay informational; on a local rung the observed-window average uses first-transmission DATA pps per output against the fixed ~760. A stall (zero delivery in a sample) is an immediate per-sample failure | rate |
| connection churn | `acceptedPerSec` / `closedPerSec` on each peer during the rated window; any re-accept or close means the steady state was not steady | rate |
| budget pressure (instantaneous) | `budgetExhaustions` (shard), `txInFlight`, `txCapacity`, `callerInFlightHwm`, `callerQueuedHwm` | gauge |
| CPU | `/proc/<pid>/stat` delta (restream process only; control plane included, reported separately from ffmpeg) | rate |
| RSS | `/proc/<pid>/status` VmRSS, plus smaps attribution | gauge |
| cycles/packet | not measurable on the reference hosts (no PMU); `cpuMicrosPerSrtPacket` is the portable stand-in | proxy |
| kernel drops | `/proc/net/snmp` `Udp:` `InErrors`/`RcvbufErrors`/`SndbufErrors` delta | rate |
| NIC drops | `/sys/class/net/*/statistics/{rx,tx}_dropped` delta, loopback excluded | rate |
| shard load | `capacity` `ingressPps`/`egressPps`/`mediaBps`/`hottestShardUtil`/`activeLeaves` and `flow` (`queue`, `backlogSlope`, `deadlineSlackMs`, `delayMs`, `errors`, `amplification`, `status`) | gauge |
| remote peer drops | each configured peer's `GET /state` deltas over its own interval: kernel `udpInErrors`/`udpRcvbufErrors`/`udpSndbufErrors` plus non-loopback NIC `rx_dropped`/`tx_dropped`, alongside accepted connections and payload bytes. The UDP figures are host-wide (the same semantics as the local row), not per-socket counters | rate |
| rated window | `ratedSecs` per sample, from the common post-prime barrier to that sample; the rung records `commonRatedWindowSecs` | gauge |
| SQEs/submission, io_uring enters/s | not sourced yet: the Compio runtime's ring counters are not exposed | gap |

The `unavailable` block in `packet-contract.json` carries each gap with its
reason, so a rung cannot silently report a zero where a metric is missing.

Each rung also carries `baselineEligible`, deliberately separate from the
runtime verdict: `healthy` describes the datapath, eligibility describes
whether the artifact may be recorded as a contractual baseline. It requires:

- a clean known SHA, an unoverridden `RESTREAM_BIN` (the provenance stamp only covers the default sibling binary), and bench binaries built from that same clean tree —
  `scripts/build/bench-harness.sh` writes `target/bench/build-provenance.json`
  next to the binaries, and a clean SHA at run time alone never proves the
  executed binary came from it;
- `lifecycle=isolated`, `peerMode=sink`, exactly one configured egress rung
  equal to the summarized rung, the canonical scenario filter, and
  `settleSecs >= 10`;
- the canonical workload (`h264-srt` ingest, `srt-source` egress, no
  transcode, `8M`) and an output count on the `{100, 300, 500, 1000}` ladder,
  with non-loopback sink peers for the 300/500/1000 rungs (loopback targets are rejected, so "remote" is mechanically true);
- a common rated window of at least ten seconds, **every** sample rated (a primed rung rates all of them, so `no-rated-samples` can never promote), and runtime `healthy`;
- for the external-host lane (`WI3.4B`), a non-loopback peer host **and**
  `topology.sameHost == false`, so a same-host `netns-veth` peer cannot stand in
  for a second machine.

So a promotion cannot happen by hand-editing the ledger, and a stale bench
pair or a convenient non-canonical run cannot masquerade as a baseline.

Each rated sample and each rung also carries an explicit `validity` verdict:

- `healthy` — every participant present, the active output count exactly equal
  to the rung's declared count, and no drop/stall pressure observed. For a
  remote rung this additionally requires same-window telemetry from *every*
  configured peer, with zero peer-side drops: a missing or unreadable peer
  reading is a reason, not a pass.
- `contaminated` — the rung was measured, but the host or the peer dropped
  datagrams, the scheduler hit a pressure signal (kernel UDP errors, NIC
  drops, driver budget violations, queue overflows, service-budget
  exhaustion), the rated window is shorter than the ten-second minimum, or
  `srtDataRetransmitPps` was nonzero. The target is a healthy **no-loss**
  path, so any retransmission in the rated window is end-to-end loss evidence:
  zero is the condition, no percentage threshold is invented. Numbers are
  still recorded; they are not a no-loss baseline.
- `invalid` — the rung cannot be compared with anything: no live SRT
  shard/owner, an active output count that is not exactly the rung's declared
  count (fewer *or* more live leaves), a non-healthy shard state, output
  retries, a rated-window feed resync, an Owner fault, rated-window Owner TX
  failures, connection churn on a peer, or a remote sink that restarted
  mid-rung.

A metric whose source is not observable is itself a reason, so "no sensor" can
never be mistaken for "sensor says zero". Retransmission share is reported
alongside for context, but the verdict follows the drop/stall/fault classes
above, not a threshold invented for retransmissions.

### 10.3 Contract rules

- The rated window starts at the common post-prime barrier: the runner polls
  every configured peer first and reads `/metrics/system` after those polls
  return, immediately after the settle period and before the first rated
  sample. No evidence is spent creating a baseline, peer drops during the
  first interval are inside the rated window, and `commonRatedWindowSecs`
  measures the same interval for the local counters and the peers. The window
  is measured, not inferred from configuration, and a rung shorter than ten
  seconds is not baseline-eligible.
- Counters are only comparable inside one rung: a `(scenario, output count)`
  change resets the sampler's history, so every rung's first sample is a
  baseline rather than a rate differenced against the previous rung.
- Each peer runs one fresh sink process per rung (`srt-sink`), because its
  counters are cumulative from process start.
- A rung is only *recorded* as a baseline with the declared workload, sink
  peers, a settle window, a sample window of at least ten seconds, a
  `packet-contract.json` whose rung verdict is `healthy`, and the run's git
  SHA (clean tree) embedded in that artifact. For a remote rung, `healthy`
  additionally requires peer telemetry proving the peer side was lossless for
  the same window. A `contaminated` rung is evidence about the local
  environment, not a performance baseline.
- Numbers are recorded in
  [quality baselines](agent-guidance/quality/baselines.md) with date and
  commit; Criterion's `target/criterion/` remains scratch.
- This tranche measures. No code may be tuned against a rung before the
  contract is frozen and a reproducible local reference artifact exists for the
  current tree — that gate is "contract frozen + local reference recorded", not
  "a remote host was available".

## 11. WI3.5 — Packet-I/O Substrate Shootout

Status: DONE at `d6145413` — the current-host substrate characterization is
recorded and the program moves on. Two single-host lanes exist: the veth/CPU-partitioned lane
(`scripts/harness/veth-topology.sh`, end-to-end, receiver in a namespace) and a
TX-only lane (`scripts/harness/dummy-lane.sh`, disposable dummy netdev, no peer,
no receiver process). Harness mode `substrate-pps` runs three arms on exactly one
pinned sender CPU — `sendto` (blocking `libc::sendto` control), `compio`, and a
native `io_uring` ring with `SUBSTRATE_REAP_MODE=sliding|window` — with an explicit
pause barrier so sender and peer counters cover the same interval, and with netdev
TX accounting required to reconcile with the sender's completions on the TX-only
lane.

SQPOLL and `SEND_ZC` are **not completion requirements**: they are optional,
non-blocking deferred experiments, to be opened only if a later stage (WI3.6's
attribution, or WI3.7's scaling) shows the submission path itself is the cost.
AF_XDP is not in this matrix at all — see WI3.8 (`§18`).

Measured (evidence in `docs/agent-guidance/quality/baselines.md`, profile under
`.local/artifacts/wi3-dummy-prof/`):

| lane | arm | pps/core |
|---|---|---:|
| veth | compio / io-uring sliding / bare `sendto` | 139 853 / 114 061 / 111 467 |
| TX-only | `sendto` / compio / io-uring sliding / io-uring window | 262 777 / 256 896 / 224 283 / **322 918** |

veth receive processing materially participates in the veth ceiling (it is roughly
half the TX-only rate, and `rps_cpus` moves the ceiling to the receiver), but even
with no peer at all the sender tops out at ~0.26-0.32 Mpps/core with ~95 % of its
CPU in the kernel transmit path. The profile shows the cost spread across skb
allocation and header build (19 %), neighbour plus device transmit (19 %), route
lookup (10 %), IP ID selection (6 %), dst release (5 %), with no netfilter (0.7 %)
or qdisc tax: there is no single hotspot, and the submission API is worth tens of
percent (batched native ring +44 % over its sliding arm, +23 % over bare `sendto`),
not multiples.

Consequence for the 2 Mpps/core threshold: 2 Mpps/core is 0.5 us/datagram against a
measured ~3.1 us of kernel transmit cost per datagram **on this measured host**
(6-vCPU Zen-class VM), so the threshold cannot be met by a better submission API
here — it requires a fundamentally cheaper transmit mechanism (AF_XDP/XDP TX or
equivalent) or a different host/kernel configuration, and it is a *sender-side*
figure that the SRT protocol work then adds on top of. Physical-NIC/RSS/XPS/IRQ/DMA
and true line-rate claims remain reserved for WI3.4B and WI3.7-final, and no
NIC-path cost per datagram is claimed here.

Wording constraint until WI3.5B runs: **the measured current host is limited to
~0.3 Mpps/core**. That is not a statement about Linux UDP in general, and it must
not be written as one; whether a modern P-core host is 2x or 4x faster is a
cross-host question, deliberately deferred (`§11.1`).

Topology: sender pinned to dedicated CPUs, receivers in network namespaces
over veth, pinned to disjoint CPUs (`§10.1a`). The receivers are cheap UDP
drain sockets, not SRT: this work item deliberately excludes the protocol so it
measures submission cost. Run the current Compio path and a native fixed-slot
io_uring path first and establish pps/core before adding further variants
(batching, SQPOLL, SEND_ZC, AF_XDP copy-mode). Classify the result as
single-host development qualification; the physical-NIC/NIC-queue claims belong
to the multi-host lane.

Purpose:

Isolate packet submission cost from SRT.

Use:

```text
preconstructed UDP payload
~1316 bytes
1000 IPv4 destinations
one pinned CPU
```

Compare:

1. current Compio TX path — **measured** (139 853 pps/core on veth, 256 896 on
   the TX-only lane)
2. purpose-built fixed-slot native io_uring benchmark — **measured** (sliding:
   114 061 on veth, 224 283 on TX-only)
3. native io_uring with large multi-SQE batches — **measured** (322 918 pps/core
   on the TX-only lane with 64 SQEs per ring enter; the best single-core arm so
   far, and still kernel-bound at 0.14 s user of 20 s)
3a. blocking `libc::sendto` control — **measured** (262 777 pps/core on TX-only),
   the control that separates submission-API cost from kernel-stack cost
4. SQPOLL where supported — **optional, deferred, non-blocking**
5. `SEND_ZC` where supported and beneficial — **optional, deferred, non-blocking**

AF_XDP is deliberately absent: the product decision is not to adopt it (`§18`).

Do not include:

- SRT protocol
- crypto
- media rings
- Restream scheduler
- MPEG-TS
- transcoding

Primary decision threshold:

```text
normal UDP + io_uring >= 2 Mpps/core
```

If normal UDP/io_uring comfortably reaches the floor:

```text
AF_XDP remains research-only
```

If it cannot:

```text
profile exact kernel/socket cost
evaluate AF_XDP seriously
```

This is the point where AF_XDP becomes evidence-driven rather than speculative.

### 11.1 WI3.5B — Modern CPU cross-host validation (deferred to WI10)

Status: DEFERRED TO WI10. Non-blocking.

Purpose: validate portability of the current-host substrate and capacity
conclusions.

Reference to preserve: `d6145413` is the current-host substrate reference
(harness mode `substrate-pps`, TX-only and veth lanes, evidence in
`docs/agent-guidance/quality/baselines.md`). Every ~0.26-0.32 Mpps/core figure is
scoped to that measured host class.

Does NOT block: WI3.6, WI3.7, WI4A, WI5, WI6, WI7, WI8, WI9.

Blocks only: final absolute capacity claims, portable shard/default selection, and
final pps/core characterization.

At WI10, run both on the modern i9-13xxxH P-core host and on the old-Zen reference
before finalizing any default:

```text
frozen raw substrate benchmark
+
finished Restream/SRT product path
```

and answer together:

```text
How much faster is raw Linux UDP?
How much faster is srt-rs?
How much faster is full Restream?
Does shard scaling change?
Are our defaults portable?
```

Nothing in the current sequence requires knowing whether this host is 2x or 4x
slower than a modern P-core: relative architectural conclusions (where our own
overhead lives, how the architecture parallelizes) transfer, absolute capacity
numbers do not.

## 12. What io_uring Does and Does Not Give Us

io_uring reduces submission/completion overhead.

It does not bypass the normal Linux UDP/IP stack.

An io_uring UDP packet still traverses mechanisms such as:

- socket lookup
- skb construction/accounting
- routing
- UDP/IP
- qdisc
- device/NIC path

Therefore:

```text
io_uring != DPDK
io_uring != AF_XDP
```

The experiment in WI3.5 exists to measure whether that remaining in-kernel path
is actually a material bottleneck for our workload.

## 13. WI3.6 — SRT Packet Engine: Incremental Cost Above the Substrate

Status: **DONE** (2026-09-22). Stage D closed without a valid rating row;
WI3.7's provenance-clean evidence is recorded as provisional; the runtime
shard-law decision remains open in Q-025 and is deferred past transport convergence.

```text
A/B saturation attribution      complete at F=1000 and F=11
C lossless attribution fanout   F=11 (not a real-time capacity point; see risk below)
C paced absolute cost           measured (median 28.05 us / DATA-first, 30 s window)
paced A -> B                    measured (+8.152 us/datagram inclusive)
paced B -> C protocol increment UNRESOLVED / NONBLOCKING (control unsuitable)
Stage D                         closed at F=11: no valid row; CPU indicative only
                                (no positive C -> D egress increment resolved)
```

Recorded state, with evidence in
[baselines](agent-guidance/quality/baselines.md):

- **A -> B (saturation, F=1000, K=16)**: median paired delta **+1.213
  us/datagram** (difference of medians +1.155; pairs 1.069-2.222). The Owner
  compatibility execution path adds ~1.2 us/datagram on this host.
- **C, F=11**: zero missed source ticks, 250 756 first-transmission DATA
  (exactly 11 x 22 796), zero DATA retransmission, zero receiver
  protocol/kernel/datapath loss, `drain_ok`. Window-only cost median **28.046
  us per first-transmission DATA** and 22.787 us per total wire datagram;
  protocol amplification **1.2308**, entirely ACK/ACKACK (ACK 0.096-0.107,
  ACKACK 0.123-0.124 per DATA-first).
- **F=11 is a lossless attribution fanout, not a product-capacity point.**
  Only 3 of 10 completed non-malformed F=11 launches were rating-eligible, and
  the clean rows still carry first-submit lateness of p99 22-95 ms and maxima
  47-124 ms. Carry both forward as qualification risk into WI3.7 (capacity) and
  WI10 (portability); WI3.6 is not gated on fixing them.
- **Paced regime (Regime P)**: paced A (raw Compio) median 307.73 us/source
  tick and paced C (full plaintext SRT) median 308.51 us/source tick at the
  11-destination 8 Mbps shape (~23% of one core for 88 Mbps). This is a
  **whole-path** statement: the net `A -> C` difference is unresolved around
  zero, and protocol CPU cannot be isolated because A and C submit and
  materialise through different machinery (raw sends versus Owner scheduler +
  direct final-slot materialisation + 1.2308 wire datagrams per DATA). It does
  bound the aggregate: full SRT as a whole shows no large positive CPU gap over
  the raw paced control.
- **Paced `B -> C` is UNRESOLVED / NONBLOCKING.** The compatibility control
  costs 90-120 us/tick above both A and C (36.613 inclusive, 10.618 drive,
  2.413 injection, leaving 23.582 us/datagram outside the measured scopes), so
  its aggregate harness/compatibility execution dominates the differential and
  no single term of it can be charged to protocol work. A direct-slot `B'`
  control would resolve the number but is **deliberately not built** — it
  reopens the measurement loop for a number no current optimization decision
  needs. Revisit only if a Stage-D result makes protocol-versus-integration
  attribution genuinely decision-critical.
- `compio_shared_owner_qual` prints `owner.rx_mode()` of the **sender's** Owner
  caller socket; `rx_mode=Some(RawReadiness)` there is a sender-side RX fallback
  and says nothing about the independent receiver process.

Naming caution (unchanged, and now measured): Stage B is the transport/Owner
execution increment *including* the legacy compatibility copy, and it is not the
production attach path.

The question is *where our overhead lives*, not an absolute pps number. Decompose
the path and report CPU microseconds per event plus overhead ratios at each step:

```text
raw UDP                      (substrate, measured in WI3.5)
  -> Compio / Owner machinery (runtime and ownership cost)
  -> SRT protocol             (encoding, ACK/NAK, timers, crypto)
  -> Restream scheduling/media (fanout, rings, mux)
```

What transfers across hosts is the **decomposition methodology and the attribution
boundaries**, not the numbers. Microarchitecture, cache hierarchy, branch
behaviour, kernel version, frequency and virtualization all move the relative cost
of raw UDP, SRT protocol work and Restream scheduling, so the CPU-cost *ratios* are
host-specific too: a 2x/4x faster host is expected to change the ratios as well as
the absolutes. WI10 must therefore remeasure **both** the absolute costs and the
ratios on the i9-13xxxH before any of them is treated as portable.

The decomposition is the deliverable, and the absolute pps/core figure is its
*output* on whatever host runs it, not an entry gate.

### 13.1 Common-topology ladder (the experiment)

Causal comparison requires one topology. **Never subtract the dummy-netdev
raw-UDP figure from a veth/SRT figure and call the difference "SRT overhead"** —
those are different kernel paths. The dummy result stays the substrate reference
(`§11`); the incremental ladder runs on the veth lane with, for every stage:

```text
same pinned sender CPU
same veth route to the peer namespace
same receiver-unlimited fanout (established first, held fixed)
same 1316-byte payload / 8 Mbps-equivalent workload
```

```text
A. raw Compio UDP                      (substrate, on this topology)
       |
B. production Owner TX, pre-materialized datagrams
       |                               (protocol generation excluded)
C. full plaintext SRT Owner            (real SRT peer: ACK/NAK/timers)
       |
D. full Restream SRT egress            (media pipeline included)
```

Stage B measures the **production `TxEngine`/`Owner::service` execution path with
benchmark-only pre-materialized injection**. Naming caution recorded so the
attribution stays honest: `bench_push_pending` stores a `Vec<u8>` in the caller
pending queue and `Owner::service` drains that legacy path by **copying the
1316-byte packet into the reserved TxPool slot**, whereas production Stage C
materializes directly into the final slot. A -> B is therefore the transport/Owner
execution increment *including* that compatibility copy, and B -> C is not purely
"protocol CPU" (C removes the copy and adds real protocol work). The compatibility
path also **drops the pending `Vec<u8>` inside `Owner::service()`**, so the
Owner-drive scope contains scheduler/pending handling + the 1316-byte copy + that
deallocation + TxPool reservation/commit + TxEngine submission/reaping; the
injection-side allocation sits outside the denominator but its deallocation does not.
Bound both with controls — a fixed 1316-byte `memcpy` and the destruction of
pre-created 1316-byte `Vec`s — and in the instrumented allocator count deallocations
and deallocation bytes as well as allocations. Report all of it; subtract none of it. Report three CPU scopes with coarse batch-scoped
`CLOCK_THREAD_CPUTIME_ID`: injection CPU, Owner-drive CPU and inclusive sender-thread
CPU, reconciling injection + drive against the inclusive figure, and never letting
injection CPU enter the Owner denominator. The upstream helpers it needs
(`with_caller`, `bench_caller_table_mut`, `bench_push_pending`) are all
`bench-internals` APIs, and `with_caller` explicitly bypasses the production
`Owner::connect` checks, so it must not be called "the production attach path" —
Stage C is where the true production SRT attach and protocol path begins. Enable
`srt-transport/bench-internals` only for the benchmark/harness build (a harness-only
feature), never for normal Restream production builds.

Stage D CPU scopes must be like-for-like: the resource sweep's
`cpuMicrosPerSrtPacket` is whole-Restream process CPU, while stage C is a single
sender thread. Report, per stage D run, the named `egress-{shard_id}` SRT shard
thread CPU delta *and* whole-process CPU, so that:

```text
C -> D  SRT-thread delta          integration cost on the egress path
D       whole-process CPU         actual product cost
D       process - SRT threads     non-egress Restream CPU: ingest SRT +
                                  demux/mux/media/control/etc.
```

and never subtract unlike CPU scopes.

#### Stage D run contract (2026-09-22)

Stage D is the full Restream SRT egress with the media pipeline, run on the
same lane and at the same lossless attribution fanout as Stage C:

```text
fanout 11, plaintext, 1316-byte payload, 8 Mbps per output
receiver: the pinned independent `srt-bench runtime=compio mode=receiver`
          process, pinned to CPUs 2-5 inside the wi3-sink namespace
sender:   Restream, with the named `egress-*` shard TID pinned to CPU 0 and
          control/media work kept off that CPU where possible; record the
          observed TID affinity
```

Measure both scopes over the same rated window: **SRT egress-shard thread CPU**
and **whole Restream process CPU**. Record per row: first-transmission DATA,
total SRT datagrams, retransmitted DATA, control classes when available
(ACK/ACKACK/other), protocol amplification, source ticks, service
visits/actions, TX submitted/completed/in-flight, receiver/kernel/datapath loss,
and first-submit/pacing lateness.

A valid D row requires zero observed receiver/kernel loss, zero DATA
retransmission, stable 11 connections, no owner fault, and complete
window/drain reconciliation. Preserve every rejected attempt.

Run at least three clean repetitions and compare like-for-like scopes:

```text
C full-SRT sender CPU / DATA-first   vs   D egress-thread CPU / DATA-first
                                          -> integration-path increment
D whole-process CPU / DATA-first          -> actual product cost
```

Never subtract C from D's whole-process CPU. For these rejected rows, no
positive C→D egress CPU increment is resolved: the D egress-thread
measurements overlap the clean C range. The process-minus-egress value is
non-egress Restream CPU (ingest SRT plus demux/mux/media/control/etc.), not a
protocol attribution. Do not turn it into a causal layer claim without a
valid D row.

Measured (2026-09-22, F=11, full detail in
[baselines](agent-guidance/quality/baselines.md)):

```text
rating-eligible D rows          0 of 5 (both recovery fences failed:
                                zero DATA retransmission and receiver
                                protocol loss with sec_b > 0; every other
                                condition passed, reconciliation exact)
D egress shard threads          median 26.018 us / DATA-first (22.857-30.111)
D whole Restream process        median 33.718 us / DATA-first (30.395-38.379)
D non-egress Restream CPU       ~7.9 us / DATA-first (process - egress;
                                ingest SRT + demux/mux/media/control/etc.)
C full-SRT sender (1 thread)    28.046 us / DATA-first (27.591-30.403)
```

No positive C→D egress CPU increment is resolved: the rejected D
egress-thread measurements overlap the clean C range. These CPU values are
indicative only. D's egress is carried by two shard threads against C's one,
and C must never be subtracted from D's whole-process CPU.

Apparatus findings that must not be re-derived: the receiver's default 250 ms
datapath horizon (189 packets/connection) is smaller than the product's
per-visit burst (`RESTREAM_EGRESS_VISIT_MAX_BYTES` = 256 KB = 199 datagrams),
so Stage D runs the receiver with `--datapath-queue-horizon-ms 4000`; the
product's burst-driven egress shows a small NAK/retransmission floor
(3-47 per 30 s window, 1.2e-5..1.9e-4 of DATA) with zero drops at every
measured layer (host veth, namespace veth, softnet, receiver socket, receiver
queue).

The bounded diagnostics closed the visit-fragment question without another
long campaign: F=11 with `RESTREAM_EGRESS_VISIT_MAX_BYTES=1316` read back
`1316` and still produced 131 DATA retransmissions / `sec_b=149`; F=1 with
the normal `262144`-byte bound read back `262144` and still produced one
retransmission / `sec_b=1`. The first result rejects the visit-fragment
hypothesis; the second shows fanout is not necessary for the floor and is
consistent with same-peer burst/TX ordering, not proof of ownership. No
upstream `TxEngine` ordering experiment is opened.

The peer-veth `rps_cpus=0` probe showed 30 retransmissions and 30 duplicates:
multi-CPU RPS is not a necessary cause, but RPS and network scheduling are not
thereby irrelevant. The paced control had zero sender retransmissions but
10 receiver duplicates; that demonstrates receiver duplicates can occur
without sender retransmission, but does not assign the D retransmissions to
the receiver. The old 64 KiB probe remains inconclusive because its artifact
did not read back the effective bound.

Reuse before inventing: pinned `srt-rs` already carries the hooks —
`crates/srt-transport/benches/compio_tx_allocs.rs` shows the benchmark-only
`Owner::new(..).with_caller(OwnerCallerSide::new_single(..))` +
`owner.service(now, budget)` shape and the pre-materialized-datagram push (Layer 3b),
and `crates/srt-bench/benches/compio_shared_owner_qual.rs` is a two-process real-`Owner`
qualification with an independent receiver process. Extend those shapes rather than
building another synthetic Owner — and note that neither is an "attach path":
`with_caller` is benchmark-only and bypasses production `Owner::connect`.

Fanout first: find the highest fanout at which the SRT sink is provably not the
limiter, and hold it for A/B/C/D. WI3.6 is about attribution, not maximum fanout.

`receiver-unlimited` has a fixed meaning here and it is not a threshold: **zero
observed receiver/kernel drops and zero retransmitted DATA during the rated
window**. Loss recovery changes the protocol cost being attributed, so a residual
allowance would corrupt the measurement it is meant to enable.

Measured (2026-09-21), on the RPS-partitioned lane (sender CPU 0, harness CPU 1,
receiver and peer RX on CPUs 2-5, peer `rps_cpus` = `3c` requested and observed,
effective socket buffers read back rather than assumed — 32 MiB requested, 50 MiB
granted):

| Fanout | Peer delivery | Coverage | Drops in the rated window |
|---:|---|---|---|
| 1 | 0.97 of 1 MB/s | 1.02 | 48, 50, 88, 45, 64, 347 per second across 6 samples |
| 2 | 1.95 of 2 MB/s | 1.02 | 131, 7.5 per second |
| 4 | 3.94 of 4 MB/s | 1.05 | 41, 18 per second |
| 8 | 7.86 of 8 MB/s | 1.02 | 18, 113 per second |
| 10 | 9.99 of 10 MB/s | 1.02 | 113 per second plus 2.91 retransmissions/s |

No fanout is lossless, including fanout 1, while the same lane's cheap UDP drain
absorbed 150 000 pps with zero drops. Simple socket-buffer undersizing is not
supported by the probe evidence (the probe is indirect and does not read the
listener sockets; prove the real listener grants from pinned srt-rs
`socket_buffer_stats()` before closing the question), so the sink drain path remains
the suspected apparatus failure. **Retire the harness `srt-sink` from WI3.6
attribution**; do not repair it unless later work needs it independently.

Replacement receiver for stages C/D: the *receiver* of the pinned upstream
two-process shape, launched inside the same namespace on CPUs 2-5:

```text
srt-bench runtime=compio mode=receiver <port> <duration> 120 --connections <N>
```

Note the naming: `compio_shared_owner_qual` is the *sender* benchmark; the
independent receiver process above is the part to run. Upstream qualification
reconciled receiver DATA with zero receiver loss through F=200 in that shape, so it
is a far better candidate than repairing the hybrid Tokio/`HighResWaiter` sink.

Stage A controls, clean (RPS lane, backlog 1 000 000, 4 drain threads, exact
`received == completed`, zero UDP/NIC/softnet drops, 3 runs each): `compio`
(frozen, boxed) 175 026 pps/core = 5.71 us/datagram median; `compio-pipeline` (no
per-datagram boxing) 171 973 = 5.82; `sendto` 176 569 = 5.66. **No reproducible boxing
penalty**: the earlier ~4 % gap did not reproduce under the corrected lossless
apparatus, so it cannot be attributed to boxing — with three attempts per arm (2/3
clean for `compio-pipeline`) this is an engineering conclusion, not a statistical
one, and boxing is simply not an optimization target. The clean Stage-A baseline for
the ladder is ~5.7 us/datagram (about 175 000 datagrams/s on one pinned core) under
the fence, and Stage A -> B measures *Owner/TxEngine execution cost*, not harness
allocation.

Boundary correctness: sender quiescence (stop refilling, drain in-flight to zero,
then acknowledge) **plus receiver settlement** — after each boundary the harness
polls the peer until its datagram delta equals the sender's completed count, exits
immediately as loss if any drop counter increments, and never charges settlement
time to sender CPU or rated wall time. `settlement.window.outcome` must be `settled`
for attribution, and the ladder requires exact reconciliation (`received ==
completed`) with zero UDP/NIC/application/softnet drops; the `<= 0.001` tolerance
survives only for historical WI3.5 reproduction.

Receiver-side drops are diagnosed, not inferred: the drain peer serves a `softnet`
block from `/proc/net/softnet_stat` and the artifact records its deltas, while
`netdev_max_backlog` is a recorded host-wide lane knob (save, set, read back,
restore on `down`) because it is not namespaced on this kernel. With 1 000 000 the
netdev drops disappear (`nicRxDropped` 0) and the ceiling moved to the drain socket
(185 429 pps/core with 0.68 % UDP drops) — **superseded**: the hardened drain
(per-thread counters, `recvmmsg` batch 32, four threads) plus the receiver
settlement fence produced clean, exactly-reconciled saturation rows, so no further
drain capacity work is needed.

Lane placement for sender attribution: sender CPU 0, harness/control CPU 1,
receiver and peer-side RX processing on CPUs 2-5, with the peer veth's `rps_cpus`
set to the receiver mask and both the requested and the observed mask recorded in
the topology env and every artifact. Plain-veth numbers predating this placement
are exploratory, not subtraction anchors.

Load regime matters as much as fanout: stage A saturates at 6.65 us/datagram of
sender CPU on this lane, while stage D at 10 outputs reports 55 us per SRT packet
because it is paced and wakeup-bound far below saturation. WI3.6 therefore runs two
explicit regimes, both at the identical selected fanout:

```text
Regime S — saturation            Regime P — product paced
  A raw UDP                        A raw UDP
  B Owner pre-materialized TX      B Owner pre-materialized TX
  C full plaintext SRT Owner       C full plaintext SRT Owner
                                   D full Restream
Question: intrinsic per-packet   Question: real 8 Mbps/output cost
cost and maximum service demand  including pacing/wakeup behaviour
```

Paced A/B must reproduce the product's **burst shape**, not a uniform drip: every
~1316 us source tick, emit one datagram per destination, so ten outputs look like
ten sends per tick rather than one send every ~132 us. Uniform spacing would give
the controls different wakeup and batching opportunities from the stages they are
compared against.

Report per stage: CPU us per source tick, CPU us per first-transmission DATA
packet, CPU us per total datagram, `total datagrams / DATA-first`, user/system CPU,
wakeups/context switches, service visits and actions, TX submitted/completed/in
flight, retransmits and peer loss. Attribute A->B to Owner/runtime, B->C to SRT
protocol and timer/control work, and C->D to Restream media/scheduler integration —
only within the same topology, fanout and regime.

Report, per stage, over repeated windows (median/min/max):

- CPU microseconds per **first-transmission DATA packet** (product capacity cost)
  and per **total SRT datagram** (packet-engine efficiency) — both denominators,
  because they answer different questions;
- `total datagrams / DATA-first` (protocol amplification, so control and
  retransmission traffic cannot masquerade as CPU inefficiency);
- DATA / ACK / NAK / retransmit rates, service visits and actions, TX
  submissions/completions/in-flight, allocation rate;
- datapath-thread CPU and whole-process CPU separately for stage D.

Commit the decomposition and its clean baseline before optimizing anything; the
incremental costs decide which layer is worth changing.

Optimization candidates must be measurement-driven.

Expected candidates:

- no Future/allocation per datagram
- fixed long-lived TX engines
- fixed packet slots
- direct final-buffer SRT encoding
- direct encryption into kernel/NIC-consumed buffer
- dense caller/session storage
- exact ready-ID queues
- batch ready destinations
- large SQE submission batches
- fewer ring submissions
- reduced wakeups
- timer aggregation
- pacing aggregation where protocol-safe
- contiguous completion metadata
- no HashMap/string work in packet hot path
- low-cardinality counters
- NUMA-local memory
- shard/NIC queue affinity

Do not weaken:

- pacing correctness
- retransmission semantics
- ACK/NAK behavior
- loss recovery
- bonding
- encryption
- slow-peer isolation

to hit a benchmark.

## 14. Same-Peer GSO vs Cross-Destination Batching

These are different optimizations.

For fanout:

```text
one packet
    ->
many destination-specific sessions
```

the key optimization is:

```text
construct many destination-specific packets
    ->
fill many SQEs
    ->
submit in one userspace scheduling pass
```

This is cross-destination multi-SQE batching.

UDP GSO is mainly useful when multiple packets are destined for the same peer
and may legally be emitted together.

At 8 Mbps / ~1316 bytes:

```text
~760 packets/s
~1.3 ms between media packets per destination
```

so ordinary healthy SRT pacing offers relatively little natural same-peer GSO
opportunity.

Do not distort SRT pacing merely to manufacture GSO batches.

## 15. Zero Copy

Benchmark zero-copy rather than assuming it wins.

At ~1.3 KB packet size:

- zero-copy setup cost
- completion bookkeeping
- notification cost

may exceed one ordinary memory copy.

Evaluate:

- `SEND_ZC`
- registered buffers
- fixed buffers
- direct final packet construction

with actual cycles/packet evidence.

## 16. WI3.7 — End-to-End Multi-Shard 8 Mbps Capacity Qualification

Status: PROVISIONAL EVIDENCE COMPLETE — the provenance-clean closure records
current-host results, but does not freeze a production coefficient or decide
the runtime shard law. The clean run selected one build provenance for all 28
plaintext cells and six crypto repeats; every selected cell was apparatus-valid
and `stable-unclassified`. Its joint fit is
`fixedCorePerShard = 0.099466`, `secondsPerData = 14.1026 us/DATA`,
`R² = 0.89508`, with a provisional two-shard upper bound. That fit is materially
different from the prior local-only `0.0518` / `20.25 us/DATA` result and is
retained as evidence, not a frozen current-host coefficient. No production
shard policy or capacity constant changes from this result. WI3.7 evidence
collection is complete for this migration; further shard-law measurement is
deferred to Q-025 after transport convergence and runtime/host qualification.

The prior local-only result is retained as an audit trail, not as the current
baseline. Its fit was approximately `fixedCorePerShard = 0.0518`,
`secondsPerData = 20.25 us/DATA`, and a two-shard upper bound. The clean
closure result above does not justify treating those coefficients as
reproduced on this host.

WI3.6 remains the strict, lossless handoff. Its `retx == 0` and duplicate-free
fences are not silently weakened. WI3.7 uses a separate `CAPACITY_MODE` arm:
retransmits and receiver duplicates are measured quality outputs, not row
rejection conditions. The row is apparatus-valid only when:

- all requested SRT connections are established and stable for the rated
  window;
- receiver UDP/kernel, veth, softnet, datapath-queue, local-drop, and retry
  counters are readable and zero;
- no SRT Owner fault, failed send, short send, protocol output failure, or
  bounded product queue overflow occurs;
- the requested shard count equals the observed `/metrics/system` shard-index
  set, and every shard index `N` reads back pinned to configured CPU `N`;
- whole-run receiver DATA and product `txClass.dataFirst` conservation is
  internally consistent.

Missed ticks, service lateness, TX-pool exhaustion, queue backlog, retransmits,
duplicates, and control amplification are recorded outputs. They classify a
sender-capacity knee; they do not make an otherwise valid saturated row
disappear. A receiver apparatus limit stops the arm separately from a
sender-saturated result.

### 16.1 Measurement seam and topology

WI3.7 is default-off:

```text
RESTREAM_BENCH_FEATURES=wi37-shard-bench
RESTREAM_WI37_SRT_SHARDS=1..4
```

The feature provides the exact requested SRT shard override only for the
benchmark arm. Normal builds retain the production CPU-derived law
`clamp(effective_cpus, 2, 8)`. The artifact records the requested count,
actual metrics indices, matched TIDs, requested CPU list, observed affinity,
and the pinning method/error. A requested/observed mismatch rejects the row;
the production policy is not changed by this work item.

Each cell keeps the product path intact:

```text
8 Mbps SRT ingress
    ->
Restream media pipeline
    ->
fanout SRT outputs
```

The egress-duty harness accepts a shard CPU list, pins shard index `N` to the
Nth configured CPU, and records per-shard Owner/service/TX counters. The fixed
six-vCPU topology isolates shard CPUs from the control, harness, and receiver
peer CPUs. Control-plane Restream and the egress-duty harness intentionally
share one control CPU; the artifact records that shared set explicitly.

### 16.2 Bounded fanout ladder and stop rules

Run each requested shard count `1, 2, 3, 4` over this ladder only:

```text
10, 20, 30, 40, 50, 60, 80 outputs
```

The bounded runner is:

```sh
scripts/harness/wi37-capacity.sh
```

Use `WI37_SHARDS=1,2,3,4` and `WI37_FANOUTS=...` to select a resumable subset.
Set `WI37_RESUME=1` to skip only a complete artifact whose contract matches the
current git revision, feature build SHA, topology, and cell configuration.
Mismatched or interrupted cells are written below `attempt-*`; earlier
artifacts are never overwritten.

Summarize the retained cells and fit provisional service demand with:

```sh
scripts/harness/wi37-capacity-analysis.py .local/artifacts/wi37-capacity \
  --provenance .local/artifacts/wi37-capacity/provenance.json \
  --out .local/artifacts/wi37-capacity/summary.json
```

The analysis consumes only artifacts whose sibling `contract.json` matches the
explicit provenance selection. It retains every matching attempt; each logical
`{shards, fanout}` cell is reduced to a median with attempt count/min/max.
Stable/sender classification flips are marked `boundary-unstable` and excluded
from the stable service-demand fit.

Stop the current shard arm at the first receiver-apparatus-limited row or
sender-clearly-saturated row. Do not run the old `100 / 300 / 500 / 1000`
matrix on this host: it is receiver-limited and cannot answer the WI3.7
sender question.

Per cell, retain:

- offered bitrate and first-DATA payload rate, in packets/s and Gbps;
- summed egress CPU, CPU seconds per DATA, hottest-shard utilization, and
  hottest/coolest shard imbalance;
- whole-process CPU and non-egress CPU;
- Owner service visits/actions, service budgets, duration sums/maxima, and
  per-shard TX counters;
- TX submitted/completed/in-flight/high-water values and exhaustion counts;
- exposed lateness p50/p99/max, missed-tick/budget violations, and backlog;
- DATA first/retransmit counts, duplicate/receiver quality values, wire
  datagrams, and control amplification;
- receiver CPU time, queue capacity/peak/full/drop values, UDP/kernel/veth/
  softnet/datapath/retry counters, and the apparatus verdict.

The artifact carries a machine-readable classification:

```text
receiver-apparatus-limited
sender-saturated
stable-unclassified
```

### 16.3 Capacity analysis

The capacity knee is selected from demand signals, not from `txInFlight > 0`
or `callerQueuedHwm > 0`: `txInFlight` is a steady-state pool gauge and a
caller queue high-water mark records admission history. Once all requested
outputs are established, caller-pool queue state is not media-send backlog.
TX exhaustion, ready-queue overflow, or admission backlog are definite
signals. Owner budget exhaustion remains pressure telemetry; it classifies
sender saturation only when persistent pressure is accompanied by observable
delivered-rate degradation.
Receiver-apparatus limits remain separate from sender demand saturation.

Fit CPU demand directly, excluding receiver-apparatus-limited and
sender-saturated rows from the provisional fit:

```text
egress_core_equivalents = egressThreadCpuSecs / windowSecs
DATA_rate                = dataFirst / windowSecs

egress_core_equivalents =
    fixed_core_per_shard * shard_count
    + seconds_per_DATA * DATA_rate
```

The analysis artifact reports the joint coefficients, per-arm affine fits,
point count, residual sum of squares/RMSE, and `R²`. It reports no coefficient
when the rows are insufficient or rank-deficient. Required shards are derived
from variable CPU demand:

```text
required_shards >= ceil(
    DATA_rate * seconds_per_DATA
    / (target_utilization - fixed_core_per_shard)
)
+ fixed-overhead and tail-latency guard
```

The coefficient and shard count are provisional WI10 evidence, not production
constants. Extra shards still require a measured CPU/tail-latency gain large
enough to pay their fixed wakeup, runtime, and cache cost. WI3.7 MUST NOT
replace the CPU-derived production shard law.

### 16.4 Bounded crypto cells

After plaintext establishes the service-demand law, run AES-128 and AES-256 at
the same receiver-safe point, three times each, alternating mode order between
repeats. The runner stores each repeat separately and the analysis reports
median/min/max for egress CPU, process CPU, service demand, and receiver CPU,
plus paired deltas. If the observed delta/noise spans zero, mark the crypto
increment unresolved on this host and defer it to WI10; do not run a full
crypto/fanout matrix unless these bounded repeats show nonlinear behavior.

Crypto cells use the same product output path and remain separate from the
plaintext shard comparison. No production `SrtCpuParallel` policy change and
no Q-025 closure is allowed from this host-only qualification.

## 17. Re-Derive the SRT Shard Law

Q-025 remains open and deferred until WI4A–WI6 transport convergence, WI7/WI9
cleanup, and WI8 runtime/host calibration are complete. WI3.7 remains
provisional evidence only; it does not select a runtime shard law.

Existing shard policy was derived under an older libsrt/CSndQueue model.

That rationale no longer holds under:

```text
one shard thread
one Compio runtime
one Owner/family
shared sockets
```

After those prerequisites, re-evaluate the policy using evidence from the final
transport topology; do not rerun WI3.7 or change production defaults as part of
the current transport-convergence package:

```text
EgressShardProfile::SrtCpuParallel
```

The replacement must be a capacity-based law, not a count fitted to one host:

```text
required shards ~= packet/event demand / measured per-shard capacity
```

with per-shard capacity a measured coefficient (WI8's Oracle should carry it as a
host measurement, not a constant), and a fixed default only after the cross-host
comparison of `§11.1`. A shard count that is best on this machine is a provisional
observation until then.

Questions to answer:

- is 2 shards still the right floor?
- when does a second shard materially improve p99 service latency?
- when does another shard only add:
  - runtime overhead
  - ring overhead
  - wakeups
  - cache misses
  - duplicated scheduler work?

Do not optimize for lowest thread count alone.

Optimize:

```text
CPU efficiency
+
tail latency
+
fault isolation
+
capacity
```

## 18. WI3.8 — AF_XDP Decision Gate

Status: SUPERSEDED — AF_XDP is not adopted. Reopen only by an explicit later
capacity decision (a deliberate tradeoff review that names what capacity target
the kernel path cannot meet), never as a default path for a future agent.

The decision: Restream keeps one packet-I/O substrate (kernel UDP + io_uring) for
SRT. The current-host characterization (`§11`) shows the stock IPv4/UDP transmit
path costs ~3.1 us of sender CPU per datagram here, and that a better submission
API does not move it — but that is a *current-host* finding, and the cross-host
comparison (`§11.1`) is what would settle whether AF_XDP-style kernel bypass is
ever needed. It is not needed to proceed with WI3.6 or WI3.7.

If reopened, the question is capacity, not architecture:

```text
srt-rs protocol/Owner
    |
packet-I/O substrate
    |
    `-- kernel UDP + io_uring   (the adopted path)
```

Do not build two long-lived product backends just to preserve optionality, and do
not let a superseded gate read as pending work.

If AF_XDP is adopted, it should live below the same logical SRT Owner and
protocol state.

Do not fork SRT behavior.

## 19. AF_XDP Scope

AF_XDP is potentially appropriate for:

```text
SRT / UDP
```

because:

- SRT implements its own:
  - reliability
  - congestion behavior
  - pacing
  - retransmission
  - encryption
- the kernel UDP stack provides less high-level protocol machinery that we
  depend on

AF_XDP would move responsibilities such as:

- Ethernet framing
- IP/UDP construction
- checksums/offloads
- route/neighbor synchronization
- PMTU handling
- NIC queue ownership
- UMEM lifecycle

closer to Restream.

That maintenance burden must be justified by measured CPU/capacity improvement.

## 20. AF_XDP Must Not Become a Universal Networking Abstraction

Do not infer:

```text
AF_XDP is faster for SRT
    ->
AF_XDP should carry RTMP/RTMPS
```

That would be the wrong abstraction boundary.

## 21. RTMP / RTMPS Networking Strategy

RTMP is TCP.

The desired path is:

```text
RTMP
    |
Compio TCP
    |
normal kernel TCP socket
    |
io_uring
```

RTMPS adds:

```text
RTMP
    |
userspace TLS handshake / key derivation
    |
kTLS handoff
    |
kernel TLS record processing
    |
kernel TCP
    |
NIC
```

Keep the normal kernel TCP stack for RTMP/RTMPS.

## 22. kTLS and AF_XDP

### SRT

SRT does not use TLS.

Its encryption is inside SRT itself:

```text
SRT payload
    ->
SRT AES
    ->
UDP
```

Therefore moving SRT packet I/O from:

```text
UDP + io_uring
```

to:

```text
AF_XDP
```

does not lose kTLS because kTLS is not involved in SRT.

### RTMPS

RTMPS does benefit from kernel TLS when using normal TCP sockets.

If RTMPS were moved onto AF_XDP, Restream would no longer be using the normal
kernel TCP/TLS stack.

That means:

```text
kTLS benefit is lost
```

and Restream would also need a userspace TCP implementation or equivalent.

That is not a desirable direction.

## 23. Why Keep RTMP on Kernel TCP

Kernel TCP gives us mature implementations of:

- congestion control
- retransmission
- reassembly
- TSO/GSO
- pacing
- PMTU
- routing
- neighbor handling
- send/receive buffering
- kTLS
- NIC offload integration

RTMP is a byte stream and can amortize packet cost through:

- larger writes
- `writev`
- chunk aggregation
- TCP send buffering
- TSO/GSO

This is fundamentally different from SRT fanout, where destination-specific UDP
packets stress per-packet work.

## 24. RTMP / RTMPS Transport Convergence

WI5 and WI6 migrate transport ownership while preserving the shared fabric
scheduler, lifecycle, retry, backpressure, generation safety, TCP quality
reporting, and shutdown behavior. This is not a performance-tuning phase;
WI3.7 remains provisional and Q-025 remains deferred.

### WI5 — Compio TCP

Status: implementation complete; live qualification in progress.

- RTMP ingress uses a Compio acceptor thread/runtime for the listener and
  accepted streams, with a bounded 64 KiB duplex bridge to fixed Tokio session
  workers.
- RTMP egress uses one Compio runtime per fabric shard, owning its TCP streams
  and `PollFd` readiness; protocol I/O remains bounded and non-blocking.
- There is no production io_uring or epoll fallback; standard-TCP and epoll
  adapters exist only under `cfg(test)`.
- Preserve fixed shard ownership, pending-byte limits, generation safety,
  reconnect policy, shutdown draining, and TCP_INFO/send-queue quality.

### WI6 — RTMPS/kTLS

Status: implementation complete; live qualification in progress.

- Rustls performs the handshake over the same Compio-owned TCP path; Linux
  kTLS is required for application records.
- `KtlsState` distinguishes `NotRequested`, `Requested`, `Enabled`,
  `Unsupported`, and `SetupFailed`; `ktlsSuccess` counts only completed
  handoffs.
- Unsupported suites/capability or a kTLS setup error fail the output. There
  is no silent userspace-TLS fallback.
- `/metrics/system` exposes request, attempt, success, unsupported, error, and
  capability state; RTMPS live preflight fails explicitly if the host cannot
  support the required AES-GCM kTLS path.
- kTLS receive preserves TLS 1.3 record types; tickets are discarded after the
  buffered Rustls handoff (no session resumption), and KeyUpdate fails closed
  until the unbuffered `KernelConnection` handoff is used.
- Focused TLS 1.2/TLS 1.3 kTLS tests exchange application data and consume
  `close_notify`; the real-media fault suite remains the acceptance gate.
- Qualification covers actual RTMPS media, reconnect after receiver loss,
  stalled receivers, transport failure, and clean shutdown. It does not add a
  new CPU/byte benchmark or derive production constants.

## 25. WI4A — SRT Productionization

Status: DONE for the current productionization scope.

- Production SRT ingress and egress use the Compio `Owner` architecture; old
  native/compatibility SRT transport paths are removed.
- Benchmark-only transport overrides are isolated behind the default-off
  `wi37-shard-bench` feature; production builds do not expose those seams.
- Owner metrics and terminology are reconciled, and stale SRT compatibility
  hooks are removed.
- No SRT shard policy, default, capacity coefficient, or CPU/NUMA policy is
  changed. Q-025 remains open and deferred.

Remaining generic/native dataplane cleanup belongs to WI7; runtime/host
calibration belongs to WI8 and final cross-host qualification to WI10.

## 26. WI7 — Dataplane Shrink

Remove obsolete generic/native transport code after SRT + RTMP migration.

Candidates:

- old UDP io_uring machinery
- old TCP io_uring machinery
- fixed-file abstractions with no remaining consumer
- legacy pollers
- compatibility wrappers
- dead runtime traits

Keep only primitives with active value.

## 27. WI8 — CPU / NUMA / Capacity Oracle

The capacity model should stop reasoning primarily in:

```text
connections
```

and use:

```text
DATA pps
control pps
total packet events/s
cycles/event
Owner service us
service visits/s
SQEs/submission
completions/s
NIC queue utilization
kernel drops
feed lag
ready depth
deadline pressure
```

Useful model:

```text
required_cpu_cycles
    =
sum(
    event_rate[class]
    *
    measured_cycles_per_event[class]
)

required_cores
    =
required_cpu_cycles
    /
effective_cycles_per_core
```

The Oracle/Flow Doctor should distinguish:

```text
media bottleneck
protocol bottleneck
scheduler bottleneck
kernel socket bottleneck
io_uring submission bottleneck
NIC bottleneck
peer/network bottleneck
```

## 28. WI9 — Abstraction Compression

Only after the real dataplane is known:

- collapse duplicate transport abstractions
- move reusable primitives into stable ownership
- reduce backend LOC
- remove compatibility aliases
- simplify test seams
- keep hot code concrete where abstraction has measurable cost

## 29. WI10 — Final Qualification

This is where the deferred absolute claims are settled: rerun the frozen substrate
matrix (`§11`) and the finished product path (`§16`) on the modern i9-13xxxH P-core
host and compare both against the old-Zen reference recorded in `d6145413`, before
finalizing shard defaults, CPU/NUMA policy or any portable capacity number
(`§11.1`).

Final qualification matrix should include:

### SRT

- plain
- AES-128
- AES-256
- 8 Mbps
- 100/300/500/1000 outputs
- no loss
- controlled loss
- controlled latency
- slow application receiver
- frozen/broken receiver
- direct
- bonded Broadcast
- bonded Backup
- RawReadiness
- ManagedMultishot
- container deployment
- host deployment

### RTMP

- RTMP
- RTMPS
- kTLS
- no-kTLS reference
- slow receiver
- reconnect churn
- large fanout
- B-frame correctness

### System

- CPU affinity
- NUMA
- NIC queue affinity
- memory plateau
- shutdown
- repeated churn
- container seccomp
- resource leaks
- latency percentiles

## 30. Performance Diagnosis Tree

Use this exact order when a target is missed.

```text
                     Product misses target
                             |
                             v
                 Raw UDP/io_uring >=2 Mpps?
                    /                    \
                  no                      yes
                  |                        |
                  v                        v
        kernel/socket/io_uring        Full SRT >=1 Mpps?
          substrate problem             /          \
                                        no          yes
                                        |            |
                                        v            v
                                    srt-rs        Restream
                                   protocol/       media /
                                   packet engine   scheduler
```

Do not skip directly to AF_XDP.

Do not blame Compio without an isolated benchmark.

Do not blame SRT when raw UDP is already the limiting layer.

## 31. Benchmark Discipline

For all packet-rate work:

- pin benchmark CPU
- isolate benchmark CPU where possible
- pin NIC queue if possible
- record NUMA node
- record NIC model/driver
- record link rate
- record offload state
- record kernel
- record CPU model/frequency
- no parallel builds
- no unrelated harness load
- warm up first
- preserve raw samples
- report median/min/max
- report failure rows
- report packet loss
- report NIC drops
- report kernel drops

Never compare two transport implementations on different hosts and call the
difference causal.

## 32. Result Storage

Durable summaries belong under:

```text
test/harness/baselines/
```

Large raw artifacts belong under:

```text
.local/artifacts/
```

Performance findings should also update:

```text
docs/agent-guidance/quality/baselines.md
```

Do not commit enormous raw logs.

## 33. Existing Backlog Mapping

Keep these in:

`docs/agent-guidance/quality/backlog.md`

### Q-025

Keep the SRT shard-law decision open. WI3.7 is provisional current-host
evidence, not a production coefficient.

Resume measurement only after WI4A–WI6 transport convergence, WI7/WI9 cleanup,
and WI8 runtime/host calibration. Run the shard-law matrix against that final
topology, then use WI10 cross-host qualification before finalizing a default.
Do not rerun WI3.7 or change production policy as part of the current package.

### Q-026

Frozen-SRT-destination RSS attribution/recalibration.

Roadmap relationship:

independent resilience debt.

Do not block the Compio migration or packet-rate program on it unless evidence
changes from bounded plateau to unbounded growth.

## 34. Work-Item Tracking Convention

Each roadmap item should carry one of:

```text
PLANNED
ACTIVE
BLOCKED
DONE
SUPERSEDED
CONDITIONAL
```

When completed, record:

- Restream SHA
- srt-rs SHA if applicable
- PR number
- key architectural result
- benchmark/evidence path
- deferred debt generated by the work

Do not erase old items after completion.

The roadmap should remain a readable architectural history.

## 35. Current Sequence

The current program state and next dependency order are:

```text
WI3.1–WI3.6
    complete; WI3.5B / WI3.4B remain external-host evidence

WI3.7
    provisional current-host evidence complete; no production coefficient or
    runtime shard-law decision; Q-025 remains open and deferred

WI3.8
    AF_XDP rejected for current requirements; reopen only if final normal-socket
    evidence shows the substrate cannot meet a stated requirement

WI4A
    SRT Compio Owner productionization complete; no policy/default change

WI5
    RTMP ingress and egress Compio TCP implementation complete; live qualification
    is the active acceptance gate

WI6
    RTMPS on the same Compio path with strict kTLS handoff; live qualification
    is the active acceptance gate

Transport live CI
    short PR SRT/RTMP smoke; broader redevelop media/crypto/fault matrix;
    nightly full certification and churn/measurement lanes

WI7
    delete obsolete dataplane machinery after transport convergence

WI9
    compress abstractions after the active dataplane is known; retain a stable
    post-cleanup baseline

WI8
    runtime Performance Oracle and host calibration

Q-025
    re-derive and validate a dynamic shard model at the final topology

Continuous regression / experiment loop
    observe, diagnose, benchmark candidates, live-qualify, then adopt or drop

WI10
    final cross-host qualification before portable defaults are finalized

```

## 36. Immediate Next Action

WI3.7's current-host evidence is frozen provisionally. Do not rerun its
performance matrix, derive a new shard coefficient, or change production
defaults in this transport-convergence work.
Sequence: `WI4A -> WI5 -> WI6 -> live-CI convergence`.

WI4A SRT productionization is complete. The current milestone finishes WI5/WI6
through actual RTMP/RTMPS/SRT media and reconnect, slow-receiver, failure, and
shutdown qualification, then passes the PR/redevelop live-CI tiers. Q-025 stays
open until the final topology and WI8 Performance Oracle are ready.

After those acceptance gates, the next roadmap work is WI7 dataplane cleanup,
then WI9 abstraction compression and a stable baseline, followed by WI8, Q-025
dynamic-model validation, the continuous regression/experiment loop, and final
WI10 cross-host qualification.

## 37. Definition of Success

The SRT/Compio program is successful when all of the following are true:

Architecture:

- one SRT implementation
- one Owner model
- no permanent fallback transport
- fixed shard ownership
- bounded work
- bounded memory
- bounded queues
- no thread/task/socket per output

Correctness:

- SRT interop preserved
- direct SRT works
- bonding works
- StreamID admission works
- crypto works
- slow peers are isolated
- retries are bounded
- stale events are harmless

Operations:

- Docker deployment contract works
- io_uring failures are explicit
- RawReadiness is observable
- ManagedMultishot is observable
- no hidden runtime fallback

Performance:

- normal UDP + io_uring >=2 M datagrams/s/core
- healthy SRT >=1 M DATA packets/s/core
- total protocol work trends toward >=1.25-1.5 M events/s/core
- 8 Mbps x 1000 is qualified on >=25 GbE
- one aggregate datapath core is the stretch target
- shard count is based on measured mechanism, not old libsrt assumptions

Transport strategy:

- AF_XDP is used only if normal UDP/socket evidence justifies it
- RTMP/RTMPS stays on kernel TCP
- kTLS remains available for RTMPS
- no universal packet-I/O abstraction is created merely for symmetry
