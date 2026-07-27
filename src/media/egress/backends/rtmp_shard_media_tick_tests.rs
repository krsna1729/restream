//! Regression tests for `on_media_tick`'s connect-completion scheduling,
//! split out of `rtmp_shard_tests.rs` to stay under the source-audit line
//! cap. See the doc comment on `EgressShardBackend::on_media_tick`
//! (`src/media/egress/shard.rs`) for the full story: a freshly-connected
//! leaf has no independent way to discover its own I/O readiness — the
//! shard runtime's `on_ready`/`poll_ready` path only runs when something
//! schedules ready work, and nothing did that for a connect completing on
//! its own. On a source with any real gap before its first published unit
//! (a cold-starting transcoder, a file ingest), that left the leaf
//! registered but never visited until an unrelated `FeedWake` happened to
//! arrive — sometimes tens of seconds later — at which point the shard's
//! own stall sweep had usually already force-closed it as "terminated
//! unexpectedly", so the visible symptom was a spurious close-and-retry
//! loop rather than a hang.

use super::*;

/// Builds a backend wired to a real `RtmpResolveCompletionQueue` (not the
/// `NoopRtmpResolveCompletionSource` the other tests in this file use) so
/// this test can push a connect completion the same way the production
/// resolve worker does, then observe `on_media_tick`'s real return value.
fn backend_with_resolve_queue() -> (
    RtmpShardBackend<TcpEgressPoller, RtmpResolveCompletionQueue>,
    std::sync::mpsc::SyncSender<RtmpResolvedConnect>,
) {
    let (sender, queue) = rtmp_resolve_completion_queue(4);
    let backend = RtmpShardBackend::with_runtime_components(
        TcpEgressPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        queue,
        EmptyRtmpPublishStartupSource,
    );
    (backend, sender)
}

#[test]
fn on_media_tick_schedules_ready_work_when_a_connect_completes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    // The peer only needs to accept the TCP connection for this test —
    // completing the RTMP handshake is not the behavior under test.
    let server = thread::spawn(move || {
        let _ = listener.accept();
    });

    let (mut backend, sender) = backend_with_resolve_queue();
    let output_id = OutputId::new("media-tick-leaf");
    backend.on_command(EgressCommand::Add(output_spec(
        "media-tick-leaf",
        &format!("rtmp://{addr}/live/key"),
        1,
    )));
    sender
        .send(RtmpResolvedConnect {
            output_id: output_id.clone(),
            generation: 1,
            peer_addr: addr,
        })
        .unwrap();

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::ScheduleReady { count: 1 },
        "a completed connect must schedule ready work so the new leaf gets \
         its first readiness check without waiting on an unrelated FeedWake"
    );
    assert!(
        backend.output_sockets.contains_key(&output_id),
        "the connect must have actually produced a registered leaf"
    );
    server.join().unwrap();
}

#[test]
fn on_media_tick_is_a_no_op_when_nothing_resolved() {
    let (mut backend, _sender) = backend_with_resolve_queue();

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::Continue,
        "an idle tick with no resolved connects must not schedule ready work"
    );
}
