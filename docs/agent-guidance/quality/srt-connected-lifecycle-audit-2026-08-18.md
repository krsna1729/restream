# Connected SRT lifecycle audit — 2026-08-18

This audit answers whether the Rust connected receiver has enough handshake
and group information, and whether the connected datagram socket is created at
the correct point in the lifecycle.

## Contents

- [Finding](#finding)
- [Bugs that explained the earlier connected failures](#bugs-that-explained-the-earlier-connected-failures)
- [libsrt reference alignment](#libsrt-reference-alignment)
- [Live verification](#live-verification)

## Finding

The handoff boundary is correct after commit `895ecd5f`:

1. The public listener owns the UDP tuple and creates a `RustConnection` in
   `Listening` state.
2. It parses the CONCLUSION before admission, retaining the peer StreamID,
   peer GROUP metadata, peer SRT socket ID, SYN cookie, and caller initial
   sequence number. For a bonded leg it also installs a listener-side mirror
   GROUP extension before the Core processes CONCLUSION, so the response
   contains the mirror group ID required by libsrt.
3. The Core processes CONCLUSION, queues the response and connection timers,
   and transitions to `Connected`.
4. The listener drains the response on the public socket, assigns the tuple to
   one worker, and transfers the complete Core plus its timer state with an
   ordered `Handoff` command.
5. The worker creates, binds, connects, and registers the connected UDP socket
   before servicing the transferred Core. Subsequent packets are therefore
   handled by the owning worker and no longer require the public listener.
6. `Connected` means transport handshake complete, not application admission.
   Authorization remains asynchronous. Until it completes, the worker keeps
   the same Core in a pending route; after acceptance it promotes the route to
   a single connection or a GROUP member. This preserves queued post-handshake
   data and prevents an `Authorize` command from racing ahead of socket
   ownership.

The Core has enough information for this boundary. The tuple is the only safe
identity before the first handshake fields arrive; the peer socket ID is a
stable member identity after CONCLUSION; and StreamID plus peer GROUP ID is
the logical bonding identity. The local mirror GROUP ID is an output-side
identity and is kept in the listener cache rather than exposed as a peer
identity.

## Bugs that explained the earlier connected failures

The earlier failures were lifecycle failures, not evidence that connected UDP
itself was unsuitable:

- Handshake timers previously timed out instead of retransmitting the last
  INDUCTION or CONCLUSION packet.
- Pending routes did not service their Core timers after handoff, so a lost
  handshake response could leave the route inert.
- Handoff and authorization failure paths could leave the listener's tuple
  route occupied, preventing a reconnect from being admitted.
- Production worker routing used only `group_id`, while admission identity was
  `group_id + normalized StreamID`. Distinct publishers reusing a group ID
  could therefore be pinned to the same worker and share the mirror-group
  cache. The router and mirror cache now use the same logical key.
- Authorization is asynchronous after transport `Connected`. A GROUP leg can
  therefore have already delivered application payloads into its bounded
  pre-authorization queue before the worker receives the accept command. The
  GROUP path previously rejected that state even though the single-leg path
  drained it. Group admission now installs the member and returns the queued
  payloads to the logical session in order.

These are covered by handshake retry, route cleanup, routing tests, and a
regression test for GROUP admission with pre-authorization data.

## libsrt reference alignment

The pinned native reference was checked in
`.local/build/static/src/srt/srtcore/api.cpp` and `core.cpp`:

- `newConnection` creates the accepted socket, maps the peer, calls
  `acceptAndRespond`, marks it connected, and only then publishes the socket
  to the accept path.
- `interpretGroup` processes the caller GROUP extension during CONCLUSION,
  creates or joins the listener mirror group, and requires the listener to
  return a GROUP extension containing the mirror group ID.
- Only the first connected member is submitted as the logical group accept;
  later legs remain group-owned.

The Rust path mirrors that ordering while making socket ownership explicit:
the worker owns the connected UDP socket and all subsequent Core I/O, while
the listener retains only tuple routing and admission coordination.

## Live verification

The optimized Rust/Rust end-to-end run used both connected paths:

```text
RESTREAM_SRT_BACKEND=rust
RESTREAM_SRT_INGEST_SCALING=connected
RESTREAM_SRT_INGEST_ROUTING=least-tuples
HARNESS_SRT_SINK_BACKEND=rust
HARNESS_SRT_SINK_SCALING=connected
HARNESS_SRT_SINK_CONNECTED_ROUTING=least-tuples
```

`mixed.live.srt.h264.a1.bf0` passed all 16 output probes, HLS, recording,
stage-sharing, and lifecycle cleanup. The artifact directory is
`.local/artifacts/mixed/live/srt/h264/a1/bf0/`.

After the logical-key and GROUP pre-authorization fixes, a fresh optimized
Rust/Rust connected bonded MSR smoke also passed:

- 30/30 outputs reached the sink;
- two bonded ingest legs reached `publisher connected` and one logical
  pipeline;
- the Rust sink reported `handoffs=1`, `group_packets=2`, and
  `group_worker_reuses=1`;
- `packetsSentDrop=0` and `bytesOutDelta=7,594,725`.

The machine-readable result is
`.local/artifacts/msr-rust-bond-connected-racefix-20260818/msr.json`; the
run used the refreshed `target/bench` binaries.

The remaining proof is not the basic handoff: it is a post-fix bonded
Broadcast/Backup run with deliberately distinct source tuples, failover, and
the 300/700/1200 scale gates. Those tests must continue to report listener
assignments, worker ownership, group membership, and disconnect/reconnect
cleanup separately.
