//! Regression tests for native SRT ingress protocol and worker routines.

use super::*;
use std::net::UdpSocket;

fn test_policy_store() -> Arc<SrtIngestPolicyStore> {
    use crate::domain::srt_ingest::SrtGlobalIngestConfig;
    Arc::new(SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ))
}

fn test_listener_parts() -> (
    PeerTable,
    AdmissionOptions,
    IngressTelemetry,
    Arc<SrtIngestPolicyStore>,
) {
    // Build admission context the same way the Tokio listener does, so
    // the owner-thread protocol path is tested, not reimplemented.
    let bind = "127.0.0.1:0".parse().unwrap();
    let prepared = srt_transport::ListenerConfig::builder(bind)
        .topology(srt_transport::ListenerTopology::PerPort)
        .bonded_inputs(srt_transport::advanced::admission::BondedInputPolicy::Accept)
        .build()
        .and_then(|config| config.prepare(srt_transport::RuntimeFlavor::Mio))
        .expect("test listener prepares");
    (
        prepared.peer_table(),
        prepared.admission_options(),
        IngressTelemetry::default(),
        test_policy_store(),
    )
}

#[tokio::test]
async fn native_owner_admits_and_replies_without_tokio_protocol() {
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = receiver.local_addr().unwrap();
    let (peers, admission, telemetry, store) = test_listener_parts();
    let mut ingress = match NativeSrtIngress::start(
        receiver,
        Arc::new(ListenerSocketStats::default()),
        peers,
        admission,
        telemetry,
        store,
    ) {
        Ok(ingress) => ingress,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(error) => panic!("native UDP unavailable: {error}"),
    };
    // Raw garbage is not a valid SRT handshake: the owner admits it
    // (and drops it) without emitting control events or crashing.
    // Protocol correctness (valid handshake -> Connected event) is
    // covered by srt-rs admission tests; here we prove the owner thread
    // stays alive and keeps counting datagrams.
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.send_to(b"not-srt", peer).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(ingress.stats.recv_datagrams.load(Ordering::Relaxed) >= 1);
    // No control event for garbage input.
    assert!(ingress.events.try_recv().is_err());
    // Legacy Tokio->worker send path still works on the shared ring.
    ingress
        .outbound
        .send((sender.local_addr().unwrap(), b"reply".to_vec()))
        .await
        .unwrap();
    let mut reply = [0_u8; 5];
    sender
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    sender.recv(&mut reply).unwrap();
    assert_eq!(&reply, b"reply");
    assert_eq!(ingress.stats.sent_datagrams.load(Ordering::Relaxed), 1);
}
