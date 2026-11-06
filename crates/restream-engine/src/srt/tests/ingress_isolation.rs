//! One future-headed publisher must not starve the ingress Owner it shares.

use std::net::UdpSocket;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_proto::wire::{ControlType, SrtPacket};

use super::super::ingress_test_support::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_future_head_does_not_starve_a_shared_ingress_owner() {
    const WINDOW: u32 = 8192;
    const CHUNKS: usize = 400;
    // Duplicates the proxy keeps unreceived at the Owner; far below any socket
    // buffer, so none are lost to the kernel.
    const IN_FLIGHT: u64 = 128;
    const MIN_REPLAYS_PER_SECOND: f64 = 25_000.0;
    let mut config = crate::config::EngineConfig::from_env();
    config.srt_ingress_owners = 1;
    config.srt_max_peers_per_ip = 2;
    config.srt_ingress_window_memory_bytes = None;
    let engine = Arc::new(crate::engine::MediaEngine::new_with_config(Arc::new(
        config,
    )));
    let server = TestServer::start_with_engine(
        &["future-head", "healthy-head"],
        vec![plain("future-head"), plain("healthy-head")],
        engine,
    )
    .await;
    let remote = server.remote;
    let proxy = UdpSocket::bind("127.0.0.1:0").expect("timestamp proxy");
    proxy
        .set_read_timeout(Some(Duration::from_millis(1)))
        .unwrap();
    // Keep host scheduling from dropping hostile DATA at this userspace hop.
    rustix::net::sockopt::set_socket_recv_buffer_size(&proxy, 8 << 20).unwrap();
    let proxy_addr = proxy.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = flume::bounded(1);
    let acked = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let proxy_acked = acked.clone();
    let proxy_stop = stop.clone();
    let owner_stats = server.engine.listener_stats_handle();
    let proxy_task = tokio::task::spawn_blocking(move || {
        let epoch = Instant::now();
        let mut caller_addr = None;
        let mut first_sequence = None;
        let mut replay = Vec::new();
        let mut replay_from: Option<(Instant, u64)> = None;
        let mut replayed = 0u64;
        let mut buffer = [0u8; 1500];
        while !proxy_stop.load(Ordering::Acquire) && epoch.elapsed() < Duration::from_secs(20) {
            match proxy.recv_from(&mut buffer) {
                Ok((len, peer)) if peer == remote => {
                    if let SrtPacket::Control(packet) = SrtPacket::decode(&buffer[..len]).unwrap()
                        && packet.control_type == ControlType::Ack
                        && let Some(first) = first_sequence
                        && let Some(ack) = packet.control_info.get(..4)
                    {
                        let next = u32::from_be_bytes(ack.try_into().unwrap());
                        proxy_acked
                            .fetch_max(next.wrapping_sub(first) & 0x7fff_ffff, Ordering::Relaxed);
                        if replay_from.is_none() && next.wrapping_sub(first) & 0x7fff_ffff == WINDOW
                        {
                            // A cumulative ACK proves that the ingress Owner retained
                            // the complete window, not merely that the proxy sent it.
                            replay_from = Some((
                                Instant::now(),
                                owner_stats.ingress_owner.snapshot().rx_packets,
                            ));
                            proxy
                                .set_read_timeout(Some(Duration::from_micros(100)))
                                .unwrap();
                            ready_tx.send(()).unwrap();
                        }
                    }
                    proxy
                        .send_to(&buffer[..len], caller_addr.expect("caller address"))
                        .unwrap();
                }
                Ok((len, peer)) => {
                    caller_addr = Some(peer);
                    if let SrtPacket::Data(mut packet) = SrtPacket::decode(&buffer[..len]).unwrap()
                    {
                        let first = *first_sequence.get_or_insert(packet.sequence_number);
                        let offset = packet.sequence_number.wrapping_sub(first) & 0x7fff_ffff;
                        assert!(offset < WINDOW);
                        packet.timestamp = 100_000_000 - offset;
                        let len = packet.encode_into(&mut buffer).unwrap();
                        if replay.is_empty() {
                            replay.extend_from_slice(&buffer[..len]);
                        }
                        proxy.send_to(&buffer[..len], remote).unwrap();
                    } else {
                        proxy.send_to(&buffer[..len], remote).unwrap();
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("timestamp proxy: {error}"),
            }
            if let Some((_, rx_base)) = replay_from {
                // Keep a bounded number of duplicates in flight and refill only
                // as the Owner receives them: the rate then measures Owner work
                // per duplicate, never socket-buffer loss on a busy host.
                let drained = owner_stats.ingress_owner.snapshot().rx_packets - rx_base;
                while replayed < drained + IN_FLIGHT {
                    proxy.send_to(&replay, remote).unwrap();
                    replayed += 1;
                }
            }
        }
        (replayed, replay_from.map(|(at, _)| at.elapsed()))
    });
    let attacker_stop = stop.clone();
    let attacker = tokio::task::spawn_blocking(move || {
        let mut sent = 0;
        run_caller(
            direct(proxy_addr, "#!::r=future-head,m=publish", None),
            Duration::from_secs(20),
            move |ctx| {
                // Pace each Owner visit: a single burst of the whole window
                // overflows the caller's own bounded protocol output queue.
                let mut batch = 0;
                while ctx.connected()
                    && sent < WINDOW
                    && batch < 256
                    && ctx.send(&Bytes::from_static(b"x"))
                {
                    sent += 1;
                    batch += 1;
                }
                attacker_stop.load(Ordering::Acquire)
            },
        )
    });
    if tokio::time::timeout(Duration::from_secs(10), ready_rx.recv_async())
        .await
        .is_err()
    {
        stop.store(true, Ordering::Release);
        panic!(
            "the hostile receive window did not fill: acked {} of {WINDOW}; attacker {:?}; {}",
            acked.load(Ordering::Relaxed),
            attacker.await.expect("attacker caller"),
            server.diagnostics().await
        );
    }
    let healthy = blocking(move || {
        run_caller(
            direct(remote, "#!::r=healthy-head,m=publish", None),
            Duration::from_secs(10),
            publish_step(ts_chunks(CHUNKS)),
        )
    })
    .await;
    server
        .wait_until(
            "all healthy payloads reach Owner media",
            Duration::from_secs(3),
            || async {
                server
                    .engine
                    .listener_stats_handle()
                    .ingress_owner
                    .snapshot()
                    .media_payloads
                    == CHUNKS as u64
            },
        )
        .await;
    let stats = server
        .engine
        .listener_stats_handle()
        .ingress_owner
        .snapshot();
    stop.store(true, Ordering::Release);
    let attacker = attacker.await.expect("attacker caller");
    let (replayed, replay_time) = proxy_task.await.expect("timestamp proxy");
    let ring = server
        .engine
        .get_or_create_pipeline("pipeline-healthy-head")
        .await;
    let video_dts: Vec<_> = (0..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .filter(|packet| packet.media_type == restream_core::MediaType::Video)
        .map(|packet| packet.dts)
        .collect();
    server.stop().await;
    assert!(attacker.connected && !attacker.disconnected, "{attacker:?}");
    assert_eq!(attacker.sent_bytes, WINDOW as usize);
    assert!(healthy.connected && !healthy.disconnected, "{healthy:?}");
    assert_eq!(healthy.sent_bytes, CHUNKS * CHUNK);
    assert_eq!(stats.media_payloads, CHUNKS as u64, "{stats:?}");
    assert_eq!(stats.owners, 1, "the publishers must share one Owner");
    assert!(!stats.faulted, "{stats:?}");
    assert!(
        video_dts.last().expect("healthy video tail")
            > video_dts.first().expect("healthy video head"),
        "the healthy publisher delivers advancing video timestamps"
    );
    assert!(
        video_dts.windows(2).all(|pair| pair[0] <= pair[1]),
        "healthy video stays ordered"
    );
    assert!(
        replayed >= 1024,
        "the complete window remained under replay: {replayed}"
    );
    // Rescanning the retained window per readiness check costs ~100 µs here,
    // capping the Owner every peer shares near 10k datagrams/s; reading only
    // the head keeps it far above that.
    let rate = replayed as f64 / replay_time.expect("replay ran").as_secs_f64();
    assert!(
        rate >= MIN_REPLAYS_PER_SECOND,
        "the shared Owner received only {rate:.0} duplicates/s; {stats:?}"
    );
}
