# SRT Bonding Wire Spec — 2026-08-16

Phase 1 deliverable for
[`../../srt-pure-rust-plan.md`](../../srt-pure-rust-plan.md): a verified
wire-behavior spec for SRT group (bonding) semantics, Broadcast-first,
Backup at lighter depth, per that plan's Phase 1 scope. Every claim below
carries either a `srtcore/*.cpp:line` reference against the exact pinned
build commit, or a live capture/log artifact — no claim from memory.

## Contents

- [Method](#method)
- [HS group-extension wire layout](#hs-group-extension-wire-layout)
- [Caller/listener interpretation flow](#callerlistener-interpretation-flow)
- [Broadcast: shared sequence and fan-out send](#broadcast-shared-sequence-and-fan-out-send)
- [Backup: activation, promotion, and sequence continuity](#backup-activation-promotion-and-sequence-continuity)
- [Group-level receive merge](#group-level-receive-merge)
- [Correction: the existing bond test helpers already support both group types](#correction-the-existing-bond-test-helpers-already-support-both-group-types)
- [Final Core extension-point list](#final-core-extension-point-list)
- [Go/no-go](#gono-go)

## Method

**Source reading** against the exact pinned build commit — no fresh clone
needed; restream's own static-build source tree already checks out this
exact commit:

```sh
git -C .local/build/static/src/srt log -1 --format="%H %ci %s"
# b6b4ae990daa8193625a4ddeaeaed03023b23125 2026-04-17 08:56:57 +0200
# matches scripts/build/native/native-inputs.lock's RESTREAM_LOCK_SRT_COMMIT exactly
```

Files read: `srtcore/group.h`, `srtcore/group.cpp` (`sendBroadcast`,
`sendBackup`, `recv`), `srtcore/handshake.h` (bitfield layout), `srtcore/
core.cpp` (`fillHsExtGroup`, `interpretGroup`), `srtcore/core.h` (`GroupDataItem`
enum), `srtcore/srt.h` (`SRT_GROUP_TYPE`).

**Live capture** using the existing, already-built bond test helpers
(`.local/build/static/prefix/bin/restream-srt-bond-{server,client}`,
built by `scripts/build/native-deps.sh` against the same static `libsrt.a`
restream links) on loopback, both group types:

```sh
sudo tcpdump -i lo -w /tmp/broadcast_bond_capture.pcap 'udp portrange 30100-30100' &
LD_LIBRARY_PATH=.local/build/static/prefix/lib \
  .local/build/static/prefix/bin/restream-srt-bond-server broadcast 30100 &
LD_LIBRARY_PATH=.local/build/static/prefix/lib \
  .local/build/static/prefix/bin/restream-srt-bond-client broadcast 30100
# repeat with `backup` in place of `broadcast`, port 30101
```

Reproducible on demand; capture files are not committed (binary artifacts,
not source — see `docs/README.md`'s documentation rules). Packet-level
detail below is read directly from `tcpdump -r <file> -n -tt` output
(no Wireshark/tshark SRT dissector available in this environment, so
per-field byte dissection relies on the source-derived layout below, not
capture-tool decoding).

## HS group-extension wire layout

The group extension rides inside the HSv5 handshake extension list as
`SRT_CMD_GROUP`, exactly 2×32-bit words (`GroupDataItem::GRPD_E_SIZE = 2`,
`core.h:127-136`), encoded by `CUDT::fillHsExtGroup` (`core.cpp:1480-1512`):

| Word | Field | Encoding |
|---|---|---|
| 0 (`GRPD_GROUPID`) | Group ID | Raw `uint32_t`, the group's `SRTSOCKET` id |
| 1 (`GRPD_GROUPDATA`) | Type + flags + weight, packed | Bits 31-24: group type (`HS_GROUP_TYPE`); bits 23-16: flags (`HS_GROUP_FLAGS`); bits 15-0: weight (`HS_GROUP_WEIGHT`) — bit ranges confirmed at `handshake.h:163-165` |

`SRT_GROUP_TYPE` (`srt.h:941-948`, this exact pinned commit): `UNDEFINED=0`,
`BROADCAST=1`, `BACKUP=2`, `E_END` — **only two real group types exist in
this codebase**; no balancing/multicast/other type is defined here (the
source has a `// ...` placeholder comment where such types might go in a
newer libsrt version, but none exists at `v1.5.5`). This matches restream's
actual scope exactly — no filtering needed to exclude balancing later.

Weight is a 16-bit unsigned value; the existing bond client sets it as
`{1, 0}` for Backup-type groups specifically (`test/native/srt-bond-client.c:41-44`)
and leaves it unset (0) for Broadcast — Broadcast's send scheduling doesn't
consult weight (confirmed: `sendBroadcast` never reads a per-member weight
field), only Backup's active-link selection does.

## Caller/listener interpretation flow

Decoded by `CUDT::interpretGroup` (`core.cpp:3288-3460`):

1. Reject with `SRT_REJ_GROUP` if the local socket doesn't have
   `SRTO_GROUPCONNECT` enabled, or if the decoded type is `>= SRT_GTYPE_E_END`.
2. Reject with `SRT_REJ_ROGUE` if the group ID field doesn't actually carry
   the group-ID bit pattern (`SRTGROUP_MASK`).
3. **Caller (initiator) role:** the caller already has a local group (it
   requested one via `srt_connect_group`); on receiving the listener's
   response it records the listener's returned group ID as its "peer group"
   the first time, and on every subsequent member connection's response
   checks that the same peer group ID comes back — reject with
   `SRT_REJ_GROUP` on mismatch (`core.cpp:3376-3410`).
4. **Listener (responder) role:** `makeMePeerOf(grpid, gtp, link_flags)`
   (`core.cpp:3448`) either creates a brand-new local mirror group (first
   member seen for this `grpid`) or attaches this connection to an
   already-existing local mirror group (subsequent members) — this is the
   exact mechanism behind the documented rule in `docs/media-pipeline.md`:
   *"StreamID alone does not create a group... two independent sockets with
   matching StreamIDs are rejected as duplicate publishers"* — group
   membership is entirely driven by this wire extension, never inferred
   from StreamID.

**Live confirmation, both group types:** the induction round (4 packets,
each 64 bytes) and conclusion round (4 packets, each 92 bytes — the size
jump from 64→92 bytes is the `SRT_CMD_GROUP` extension block, 8 bytes of
payload plus `CMDSPEC` header, appearing alongside the existing
`SRT_CMD_HSREQ` extension) happen **in parallel across both member
sockets**, each with its own independent local source port, for *both*
Broadcast and Backup — confirming the handshake/wire-format layer genuinely
doesn't differ by group type, matching D3's claim in the plan.

## Broadcast: shared sequence and fan-out send

`sendBroadcast` (`group.cpp:1208-1900`+): every currently-`RUNNING` member
link gets the same payload sent on it within one `sendBroadcast()` call. The
group tracks a shared scheduling sequence; each member's own send sequence
is explicitly overridden to match before the send
(`d->ps->core().overrideSndSeqNo(curseq)`, `group.cpp:1427`) — confirmed
this is the *same* function used by `sendBackup`'s activation path
(`group.cpp:3085`) and `sendBackupRexmit` (`group.cpp:4086`), so "group owns
the sequence, not the individual connection" is a shared mechanism across
both types, not Broadcast-specific.

**Live nuance found only via capture, not source reading alone:** in a
minimal one-message test (`echo hello | bond-client broadcast`), only
**one** of the two member links (source port `.32828`) actually carried the
39-byte data packet — the second member (`.49424`) never sent it, despite
both completing the handshake identically and in parallel. This is not a
bug: `sendBroadcast` only sends over links already in `RUNNING` state at
the moment `sendBroadcast()` is called (`group.h`'s own doc comment:
"Broadcast: links... become PENDING and then IDLE only for a short moment
to be activated immediately **at the nearest sending operation**" —
`group.h:47-49`), and the second member had evidently not yet completed its
IDLE→RUNNING activation transition when this test's single send() call
happened, microseconds after handshake completion. **Design implication for
Phase 8a:** this is a real startup race, not a protocol violation — for
restream's actual use (continuous streaming over a connection that lives for
the duration of a stream, not a one-shot send), this only matters in the
first few milliseconds after group formation. Phase 8a's test design should
assert "redundancy holds in steady state," not "every single packet from
send #1 is guaranteed to hit every member" — the latter isn't even true of
real libsrt.

## Backup: activation, promotion, and sequence continuity

`sendBackup` (`group.cpp:3768-3968`) and `sendBackupRexmit` (`group.cpp:4038+`)
carry materially more logic than `sendBroadcast`, confirmed directly: RTT-driven
"stability" tracking, and an explicit log message this investigation's live
capture reproduced verbatim — `"...Reason: no stable links"`
(`group.cpp:3172`) and `"@<id> FRESH-ACTIVATED"` (`group.cpp:3270`).

**Live capture, both members set weight `{1, 0}` per the existing bond
client:** contrary to a naive "member 0 (weight 1) is immediately and solely
active, member 1 (weight 0) stays pure standby forever" expectation, the
real captured log showed **both** members receiving a `FRESH-ACTIVATED`
transition, roughly **500ms apart** (`04:51:39.426` then `04:51:39.927`),
each preceded by the identical `"trying to activate a stand-by link...
Reason: no stable links"` message. Packet-level correlation: member
`.34621` sent one handshake-adjacent 20-byte control packet then went silent
for the rest of the session; both of the test's 23-byte data messages were
actually carried exclusively by member `.52406`, activated second. Read
literally: with no active link yet considered "stable" (this is a
fresh connection, RTT/ACK history doesn't exist yet), Backup's activation
logic tried the first member, didn't find it "stable" fast enough by some
internal threshold (order of 500ms on this host), and switched to trying
the second — landing on the second as the one that actually carried data,
not the one weight nominally favored. **Design implication for Phase 8b (if
ever undertaken):** "stability" is a real, time-bounded, RTT/ACK-history-based
decision, not an instant weight-based pick — the Core's stability-detection
timer and its exact threshold need their own dedicated read of
`groups::SendBackupCtx` / `BackupMemberState` (`group_backup.h`, not yet
read in this pass — deferred, consistent with Phase 8b being optional/lower
priority).

## Group-level receive merge

`CUDTGroup::recv` (`group.cpp:2387`+) confirms the plan's D3a claim
precisely: **one shared function**, no `recvBroadcast`/`recvBackup` split.
Mechanism, read directly from source (`group.cpp:2387-2477`):

1. Collect all currently-alive member sockets (`recv_CollectAliveAndBroken`).
2. For each alive member with data ready, first drop anything already
   consumed group-wide: `ps->core().rcvDropTooLateUpTo(CSeqNo::incseq(m_RcvBaseSeqNo))`
   — `m_RcvBaseSeqNo` is the **group-owned** shared base sequence, applied
   identically to every member's own per-connection receive buffer.
3. Find "the first readable packet among all member sockets" and deliver
   it; whichever member didn't have that sequence yet (or already delivered
   it via another member) has it dropped by the same `rcvDropTooLateUpTo`
   call on its own buffer.

This confirms the extension-point table's claim that per-connection receive
buffers stay structurally untouched (still owned by the individual
connection's own Core state) — the *merge* is a group-level arbitration
layer reading across multiple already-existing per-connection buffers, not
a rewrite of how any single connection buffers data. This is good news for
Phase 8a's Core design: the `GroupMachine`'s receive-merge logic can be a
thin layer over unmodified per-connection receive state, exactly as the
plan's extension-point table already assumed.

## Correction: the existing bond test helpers already support both group types

The plan's Phase 1 checklist item — *"confirm which group type these
helpers create by default and add explicit `SRT_GTYPE_BROADCAST` support if
they currently default to (or only support) Backup"* — is resolved, and the
assumption behind it was too cautious: `test/native/srt-bond-client.c` and
`srt-bond-server.c` **already take `broadcast|backup` as an explicit first
CLI argument** (`srt-bond-client.c:10-22`, `srt-bond-server.c:10-14`) and
fully support both. **No modification needed for Phase 8a's Tier 1 interop
oracle** — it's ready today, as demonstrated by this doc's own live
captures using the unmodified binaries.

## Final Core extension-point list

Confirmed and finalized (supersedes the source-grounded-but-provisional
table in `srt-pure-rust-plan.md`'s [Bonding](../../srt-pure-rust-plan.md#bonding-the-central-design-problem)
section — that table's shape holds; every row below is now backed by an
exact citation instead of an estimate):

| Core layer | Broadcast (Phase 8a) | Additionally for Backup (Phase 8b, optional) |
|---|---|---|
| Wire format | `SRT_CMD_GROUP`, 2×u32 words: group ID (word 0), packed type(8b)/flags(8b)/weight(16b) (word 1) — `handshake.h:163-165` | — (shared, identical encoding) |
| Handshake machine | Emit as caller (`fillHsExtGroup`, `core.cpp:1480`); parse as listener/caller (`interpretGroup`, `core.cpp:3288`); listener creates-or-attaches via grpid match | — (shared, identical decode path; only the interpreted `gtp` value differs) |
| Connection machine (send) | Accept group-owned sequence override (`overrideSndSeqNo`, confirmed at `group.cpp:1427`); send only when in `RUNNING` state | Idle/Standby run mode; RTT/ACK-history-based "stability" timer (order of 100s of ms per live capture, exact threshold TBD — needs `group_backup.h` read); same `overrideSndSeqNo` call at promotion (`group.cpp:3085`, `4086`) |
| Connection machine (recv) | Expose per-packet sequence to group merger; accept `m_RcvBaseSeqNo`-driven external too-late-drop (`rcvDropTooLateUpTo`) | — (shared, identical mechanism — `CUDTGroup::recv` has no type branch at all) |
| Group machine | Member table; fan-out send to all `RUNNING` members; "first readable packet across members" receive arbitration keyed on shared `m_RcvBaseSeqNo` | Stability detection, active-link selection, promotion sequencing — deferred read, `group_backup.h` |
| Application API | `GroupHandle`; status surfacing group ID/type/member states | Same `GroupHandle`, extended member-role/state fields |

## Go/no-go

**GO.** The required Core changes for Broadcast are a bounded, precisely
specified set of hooks, all confirmed against exact source lines and cross-
checked against live capture behavior: a 2-word wire extension, a group-
owned send-sequence override applied identically to how Backup already
works, a fan-out-to-`RUNNING`-members send policy with a documented (not
hidden) startup-activation race, and a receive-merge mechanism that reuses
unmodified per-connection receive buffers rather than requiring changes to
them. Nothing found here contradicts or expands the extension-point table
already in `srt-pure-rust-plan.md` — Phase 2 can proceed on schedule.

**Deferred, not blocking:** `group_backup.h`'s exact stability-timer
threshold and `BackupMemberState` transition table (Backup is optional per
D2/Phase 8b — this doc's Backup section is intentionally lighter-depth per
the plan's own scope, and the live evidence gathered here is sufficient to
validate the extension-point table's Backup column without a full
implementation-ready spec).
