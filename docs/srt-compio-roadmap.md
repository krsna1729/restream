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
- [12. What io_uring Does and Does Not Give Us](#12-what-io_uring-does-and-does-not-give-us)
- [13. WI3.6 — SRT Packet Engine to >=1 Mpps/Core](#13-wi36--srt-packet-engine-to-1-mppscore)
- [14. Same-Peer GSO vs Cross-Destination Batching](#14-same-peer-gso-vs-cross-destination-batching)
- [15. Zero Copy](#15-zero-copy)
- [16. WI3.7 — End-to-End One-Core 8 Mbps Qualification](#16-wi37--end-to-end-one-core-8-mbps-qualification)
- [17. Re-Derive the SRT Shard Law](#17-re-derive-the-srt-shard-law)
- [18. WI3.8 — AF_XDP Decision Gate](#18-wi38--af_xdp-decision-gate)
- [19. AF_XDP Scope](#19-af_xdp-scope)
- [20. AF_XDP Must Not Become a Universal Networking Abstraction](#20-af_xdp-must-not-become-a-universal-networking-abstraction)
- [21. RTMP / RTMPS Networking Strategy](#21-rtmp--rtmps-networking-strategy)
- [22. kTLS and AF_XDP](#22-ktls-and-af_xdp)
- [23. Why Keep RTMP on Kernel TCP](#23-why-keep-rtmp-on-kernel-tcp)
- [24. RTMP / RTMPS Performance Direction](#24-rtmp--rtmps-performance-direction)
- [25. WI4 — Final SRT Cleanup / Productionization](#25-wi4--final-srt-cleanup--productionization)
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
UDP allocation-test section; the crate docs describe its surviving role (native
TCP/io_uring for RTMP, reusable scheduler/media primitives, synthetic harness).
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

Status: IN PROGRESS

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

# on the measuring host
MSR_PEER=sink RESOURCE_SWEEP_SRT_PEER_HOSTS=peer-a,peer-b RESOURCE_SWEEP_BITRATE=8M RESOURCE_SWEEP_EGRESS_COUNTS=1000 RESOURCE_SWEEP_SCENARIOS=egress-growth-source-srt WORK_DIR=.local/artifacts/wi34-ladder/1000 scripts/harness/run.sh resource-sweep -- --no-netns
```

SRT outputs are spread over the configured hosts by a stable hash of the
output name. The remote peer's own kernel drop counters live on the peer host
(the `srt-sink` mode prints them per interval and in its final artifact), so
a remote run's `packet-contract.json` names that in its `unavailable` block
instead of reporting the measuring host's numbers as if they were the peer's.

Artifacts per rung, all under `WORK_DIR`:

| File | Contents |
|---|---|
| `resource-sweep-results.json` / `.csv` | CPU, RSS, memory attribution, ring/AVIO occupancy per rung |
| `resource-sweep-samples.jsonl` | The same, one line per sample |
| `packet-contract.json` | Run metadata (git SHA/dirty, workload, peers, windows), one summary per `(scenario, output count)` rung, the validity verdict with reasons, and the `unavailable` block |
| `packet-contract-samples.jsonl` | One contract record per sample |

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
| budget pressure | `budgetExhaustions`, `serviceBudgetExhausted`, `queueOverflows`, `driverBudgetViolations` | gauge |
| Owner saturation | `txInFlight`, `txCapacity`, `txExhaustions`, `txFailedSends`, `callerInFlightHwm`, `callerQueuedHwm` | gauge |
| Owner receive pressure | `rxRingDropped`, `rxTruncated` | gauge |
| CPU | `/proc/<pid>/stat` delta (restream process only; control plane included, reported separately from ffmpeg) | rate |
| RSS | `/proc/<pid>/status` VmRSS, plus smaps attribution | gauge |
| cycles/packet | not measurable on the reference hosts (no PMU); `cpuMicrosPerSrtPacket` is the portable stand-in | proxy |
| kernel drops | `/proc/net/snmp` `Udp:` `InErrors`/`RcvbufErrors`/`SndbufErrors` delta | rate |
| NIC drops | `/sys/class/net/*/statistics/{rx,tx}_dropped` delta, loopback excluded | rate |
| shard load | `capacity` `ingressPps`/`egressPps`/`mediaBps`/`hottestShardUtil`/`activeLeaves` and `flow` (`queue`, `backlogSlope`, `deadlineSlackMs`, `delayMs`, `errors`, `amplification`, `status`) | gauge |
| remote peer drops | each configured peer's `GET /state` `udpInErrors`/`udpRcvbufErrors` delta over the same sample window, plus accepted connections and payload bytes. These are the peer host's kernel UDP counters (host-wide, the same semantics as the local row), not per-socket counters | rate |
| SQEs/submission, io_uring enters/s | not sourced yet: the Compio runtime's ring counters are not exposed | gap |

The `unavailable` block in `packet-contract.json` carries each gap with its
reason, so a rung cannot silently report a zero where a metric is missing.

Each rated sample and each rung also carries an explicit `validity` verdict:

- `healthy` — every participant present, the active output count exactly equal
  to the rung's declared count, and no drop/stall pressure observed. For a
  remote rung this additionally requires same-window telemetry from *every*
  configured peer, with zero peer-side drops: a missing or unreadable peer
  reading is a reason, not a pass.
- `contaminated` — the rung was measured, but the host or the peer dropped
  datagrams or the scheduler hit a pressure signal (kernel UDP errors, NIC
  drops, driver budget violations, queue overflows, service-budget
  exhaustion). Numbers are still recorded; they are not a no-loss baseline.
- `invalid` — the rung cannot be compared with anything: no live SRT
  shard/owner, an active output count that is not exactly the rung's declared
  count (fewer *or* more live leaves), a non-healthy shard state, output
  retries or feed resyncs, an Owner fault, Owner TX failures, or a remote sink
  that restarted mid-rung.

A metric whose source is not observable is itself a reason, so "no sensor" can
never be mistaken for "sensor says zero". Retransmission share is reported
alongside for context, but the verdict follows the drop/stall/fault classes
above, not a threshold invented for retransmissions.

### 10.3 Contract rules

- Counters are only comparable inside one rung: a `(scenario, output count)`
  change resets the sampler's history, so every rung's first sample is a
  baseline rather than a rate differenced against the previous rung.
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
  contract numbers for the current tree are recorded.

## 11. WI3.5 — Packet-I/O Substrate Shootout

Status: PLANNED

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

1. current Compio TX path
2. purpose-built fixed-slot native io_uring benchmark
3. native io_uring with large multi-SQE batches
4. SQPOLL where supported
5. SEND_ZC where supported and beneficial
6. AF_XDP zero-copy reference

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

## 13. WI3.6 — SRT Packet Engine to >=1 Mpps/Core

Status: PLANNED

Run only after WI3.5 identifies substrate headroom.

If raw UDP/io_uring can exceed 2 Mpps/core but full SRT cannot exceed 1 Mpps/core,
the remaining cost is ours.

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

## 16. WI3.7 — End-to-End One-Core 8 Mbps Qualification

Status: PLANNED

Run the actual product path:

```text
8 Mbps SRT ingress
    ->
media pipeline
    ->
100 / 300 / 500 / 1000 SRT outputs
```

Measure:

- aggregate datapath CPU
- control-plane CPU separately
- pps
- wire bitrate
- Owner service cost
- scheduler cost
- media/ring cost
- ingress cost
- egress cost
- crypto cost
- TX completion cost
- kernel networking cost

Run plaintext and encrypted variants separately.

Suggested encryption matrix:

```text
plain
AES-128
AES-256
```

The purpose is to answer:

```text
raw UDP fast, SRT slow
    -> optimize srt-rs

SRT fast, whole Restream slow
    -> optimize Restream media/scheduler

raw UDP itself slow
    -> kernel/socket substrate decision
```

## 17. Re-Derive the SRT Shard Law

Q-025 currently tracks this debt.

Existing shard policy was derived under an older libsrt/CSndQueue model.

That rationale no longer holds under:

```text
one shard thread
one Compio runtime
one Owner/family
shared sockets
```

WI3.7 must therefore remeasure and either preserve or replace:

```text
EgressShardProfile::SrtCpuParallel
```

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

Status: CONDITIONAL

AF_XDP is not the default future.

Enter this work item only if WI3.5 proves that the normal kernel UDP/socket path
prevents the packet-rate target.

Possible end-state for SRT:

```text
srt-rs protocol/Owner
    |
packet-I/O substrate
    |
    +-- normal UDP + io_uring
    |
    `-- AF_XDP, only if proven necessary
```

Do not build two long-lived product backends just to preserve optionality.

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

## 24. RTMP / RTMPS Performance Direction

After SRT is settled:

### WI5 — Compio TCP

Replace native Restream TCP io_uring RTMP machinery with Compio TCP.

Preserve:

- fixed shard ownership
- bounded pending bytes
- generation safety
- RTMP wire/state work already completed

### WI6 — RTMPS/kTLS

Retain:

- userspace TLS handshake
- kTLS handoff
- io_uring socket operation
- kernel TCP

Qualification should measure:

- plaintext RTMP
- userspace TLS
- kTLS
- vectored/coalesced writes
- TSO/GSO behavior
- CPU/byte
- CPU/connection

## 25. WI4 — Final SRT Cleanup / Productionization

Status: PLANNED AFTER PERFORMANCE DECISIONS

Once the SRT packet substrate is settled:

- remove benchmark-only hooks that should not ship
- finalize runtime metrics
- finalize shard defaults
- finalize CPU/NUMA policy
- remove stale legacy SRT wording
- remove now-unused dataplane abstractions
- shrink `restream-dataplane` around actual reusable primitives

At this point SRT should be considered architecturally complete.

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

Remeasure SRT shard-count scaling law.

Roadmap relationship:

```text
WI3.7
```

Do not close Q-025 before the one-core / shard-law matrix exists.

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

The expected execution order from the current state is:

```text
WI3.1
    upstream Owner listener admission + receiving-group identity

WI3.2
    hard-cut Restream SRT ingress to Compio Owner

WI3.3
    delete old Restream SRT transport machinery

WI3.4
    formal 8 Mbps / packet-rate benchmark contract

WI3.5
    Compio vs native io_uring vs SQPOLL/SEND_ZC vs AF_XDP substrate shootout

WI3.6
    optimize srt-rs packet engine to >=1 M DATA pps/core

WI3.7
    end-to-end 8 Mbps x 1000 <=1-core stretch qualification
    + rederive shard law

WI3.8
    AF_XDP decision only if normal UDP/io_uring misses substrate floor

WI4
    final SRT cleanup / productionization

WI5
    RTMP -> Compio TCP

WI6
    RTMPS / kTLS qualification

WI7
    delete obsolete dataplane machinery

WI8
    CPU / NUMA / Oracle capacity model

WI9
    abstraction and LOC compression

WI10
    final production qualification
```

## 36. Immediate Next Action

Do not start packet-rate optimization yet.

WI3.1, WI3.2 and WI3.3 are done. The next item is:

```text
WI3.4
```

Establish the 8 Mbps / packet-rate benchmark contract: the 100/300/500/1000
output ladder, the metric-to-source table, and the durable artifact schema.
It measures; it does not optimize.

The packet-rate program starts only after the SRT ingress and egress paths share
the same final architecture.

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
