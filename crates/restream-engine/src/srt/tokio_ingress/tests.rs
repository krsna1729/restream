//! Production control queue over controlled bounded transport endpoints.
use std::future::Future;
use std::task::{Context, Waker};

use super::super::ingress_media::ProbeIdentity;
use super::super::ingress_test_support::{direct, plain, run_caller};
use super::*;
use crate::snapshots::ListenerSocketStats;
use restream_domain::domain::srt_ingest::SrtGlobalIngestConfig;

// LogicalPeerId is deliberately opaque. Mint one through a real native
// handshake rather than forging an ID or exposing protocol internals.
async fn native_peer() -> IngressPeer {
    let mut handle = SrtIngressHandle::start(IngressConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        policy_store: Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[plain("review-queue")],
        )),
        receiver_group: ReceiverGroupId::generate(),
        stats: Arc::new(ListenerSocketStats::default()),
        command_capacity: 2,
        event_capacity: 4,
        telemetry_capacity: 1,
        owners: 1,
        max_peers_per_ip: 64,
        window_memory_bytes: None,
    })
    .await
    .unwrap();
    let remote = handle.local_addr();
    let caller = tokio::task::spawn_blocking(move || {
        run_caller(
            direct(remote, "#!::r=review-queue,m=publish", None),
            Duration::from_secs(3),
            |ctx| ctx.connected(),
        )
    });
    let event = tokio::time::timeout(Duration::from_secs(5), handle.events.recv())
        .await
        .unwrap();
    let Some(SrtIngressEvent::Connected { logical_peer, .. }) = event else {
        panic!("native handshake must produce Connected");
    };
    assert!(caller.await.unwrap().connected);
    assert!(handle.shutdown(None).await.quiescent);
    logical_peer
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_backpressure_bounds_replies_and_preserves_handoff_order() {
    let peer = native_peer().await;
    let other = IngressPeer { owner: 1, ..peer };
    let (handle, mut receivers, events) = SrtIngressHandle::test_channels(2);
    let owner_a = receivers.remove(0);
    let mut ingress = Ingress {
        handle,
        pending_command: None,
    };
    let ring = Arc::new(RingBuffer::new(16));
    let identity = ProbeIdentity {
        attempt_id: 7,
        revision: 2,
    };
    assert!(ingress.disconnect(peer).is_ok()); // Fill paused Owner A.
    assert!(
        ingress
            .command(IngressCommand::ProbeApplied {
                logical_peer: peer,
                identity,
                ring: Some(ring.clone()),
            })
            .is_ok()
    );
    assert!(ingress.pending_command.is_some());
    assert!(
        events
            .try_send(SrtIngressEvent::Probe {
                logical_peer: other,
                identity,
                probe: Box::new(DemuxProbe {
                    video: None,
                    video_sequence_header: None,
                    video_track_count: 0,
                    audio_tracks: Vec::new()
                }),
            })
            .is_ok()
    );
    for _ in 0..1024 {
        let mut next = Box::pin(Ingress::next_event(
            &mut ingress.handle.events,
            &ingress.pending_command,
        ));
        assert!(
            next.as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending(),
            "another Owner's events must not be consumed behind a blocked reply"
        );
    }
    assert!(
        events
            .try_send(SrtIngressEvent::Fault {
                detail: "still bounded".into()
            })
            .is_err()
    );
    assert!(
        ingress.disconnect(other).is_err(),
        "only one deferred command is admitted"
    );
    assert!(matches!(
        owner_a.try_recv(),
        Ok(IngressCommand::Disconnect { .. })
    ));
    ingress.flush_commands();
    assert!(ingress.pending_command.is_none());
    let Ok(IngressCommand::ProbeApplied {
        identity: applied,
        ring: Some(applied_ring),
        ..
    }) = owner_a.try_recv()
    else {
        panic!("the exact accepted handoff must survive backpressure");
    };
    assert_eq!(applied, identity);
    assert!(Arc::ptr_eq(&applied_ring, &ring));
    assert!(
        matches!(Ingress::next_event(&mut ingress.handle.events, &ingress.pending_command).await,
        Some(SrtIngressEvent::Probe { logical_peer, .. }) if logical_peer == other)
    );

    // Listener stop also preserves the already accepted handoff before native
    // shutdown; it does not allocate a per-session disconnect queue.
    assert!(ingress.disconnect(peer).is_ok());
    assert!(
        ingress
            .command(IngressCommand::ProbeApplied {
                logical_peer: peer,
                identity,
                ring: Some(ring.clone()),
            })
            .is_ok()
    );
    let drain = tokio::spawn(async move {
        assert!(matches!(
            owner_a.recv_async().await,
            Ok(IngressCommand::Disconnect { .. })
        ));
        let Ok(IngressCommand::ProbeApplied {
            ring: Some(applied),
            ..
        }) = owner_a.recv_async().await
        else {
            panic!("pending handoff must precede shutdown");
        };
        assert!(Arc::ptr_eq(&applied, &ring));
        assert!(matches!(
            owner_a.recv_async().await,
            Ok(IngressCommand::Shutdown)
        ));
    });
    let exit = ingress.handle.shutdown(ingress.pending_command).await;
    drain.await.unwrap();
    assert!(exit.quiescent, "{exit:?}");
}
