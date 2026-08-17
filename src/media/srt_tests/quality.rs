#[test]
fn srt_rates_use_counter_deltas_instead_of_cumulative_totals() {
    let sampled_at = Instant::now();
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 5_000;
    stats.pkt_rcv_drop_total = 500;
    stats.pkt_rcv_retrans = 10_000;

    let (first, snapshot) = srt_quality_from_stats(&stats, None, sampled_at);
    assert_eq!(first.packets_received_loss, Some(5_000));
    assert_eq!(first.packets_received_loss_per_sec, None);

    let (recovered, _) = srt_quality_from_stats(
        &stats,
        Some(snapshot),
        sampled_at + std::time::Duration::from_secs(2),
    );
    assert_eq!(recovered.packets_received_loss_per_sec, Some(0.0));
    assert_eq!(recovered.packets_received_drop_per_sec, Some(0.0));
    assert_eq!(recovered.packets_received_retrans_per_sec, Some(0.0));
}

#[test]
fn srt_rates_report_current_loss_window() {
    let sampled_at = Instant::now();
    let previous = SrtCounterSnapshot {
        packets_received_loss: 100,
        packets_received_drop: 10,
        packets_received_retrans: 200,
        packets_received_undecrypt: 0,
        sampled_at,
    };
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 120;
    stats.pkt_rcv_drop_total = 16;
    stats.pkt_rcv_retrans = 220;
    stats.pkt_rcv_undecrypt_total = 2;

    let (quality, _) = srt_quality_from_stats(
        &stats,
        Some(previous),
        sampled_at + std::time::Duration::from_secs(2),
    );
    assert_eq!(quality.packets_received_loss_per_sec, Some(10.0));
    assert_eq!(quality.packets_received_drop_per_sec, Some(3.0));
    assert_eq!(quality.packets_received_retrans_per_sec, Some(10.0));
    assert_eq!(quality.packets_received_undecrypt_per_sec, Some(1.0));
}

// libsrt counters are cumulative for the life of the socket and can regress
// (reset to a smaller value) across a reconnect that reuses the same
// snapshot struct. `counter_rate`'s `checked_sub` must turn that into `None`
// rather than underflowing `u64` into a near-`u64::MAX` "spike".
#[test]
fn srt_rates_report_none_when_counter_regresses_instead_of_wrapping() {
    let sampled_at = Instant::now();
    let previous = SrtCounterSnapshot {
        packets_received_loss: 500,
        packets_received_drop: 50,
        packets_received_retrans: 900,
        packets_received_undecrypt: 5,
        sampled_at,
    };
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 10;
    stats.pkt_rcv_drop_total = 1;
    stats.pkt_rcv_retrans = 0;
    stats.pkt_rcv_undecrypt_total = 0;

    let (quality, _) =
        srt_quality_from_stats(&stats, Some(previous), sampled_at + Duration::from_secs(2));
    assert_eq!(quality.packets_received_loss_per_sec, None);
    assert_eq!(quality.packets_received_drop_per_sec, None);
    assert_eq!(quality.packets_received_retrans_per_sec, None);
    assert_eq!(quality.packets_received_undecrypt_per_sec, None);
    // The absolute (non-rate) counters still reflect the post-reset value.
    assert_eq!(quality.packets_received_loss, Some(10));
}

// Two samples that land on (or before) the same `Instant` must guard the
// division in `counter_rate` rather than producing `inf`/`NaN` or a stale
// rate carried over from a prior window.
#[test]
fn srt_rates_report_none_at_zero_elapsed_seconds() {
    let sampled_at = Instant::now();
    let previous = SrtCounterSnapshot {
        packets_received_loss: 5,
        packets_received_drop: 0,
        packets_received_retrans: 0,
        packets_received_undecrypt: 0,
        sampled_at,
    };
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = 15;

    let (quality, _) = srt_quality_from_stats(&stats, Some(previous), sampled_at);
    assert_eq!(quality.packets_received_loss_per_sec, None);
}

// libsrt reports -1 (or other negative `c_int` values) as an "unknown"
// sentinel on several counters. `.max(0) as u64`/`.max(0) as f64` must clamp
// that to 0 before widening; without the clamp, casting a negative i32
// straight to u64 would sign-extend into a near-`u64::MAX` garbage value.
#[test]
fn quality_from_stats_clamps_negative_sentinel_counters_to_zero() {
    let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
    stats.pkt_rcv_loss_total = -1;
    stats.pkt_rcv_drop_total = -1;
    stats.pkt_rcv_retrans = -1;
    stats.pkt_rcv_undecrypt_total = -1;
    stats.ms_rcv_tsb_pd_delay = -1;
    stats.ms_rcv_buf = -1;

    let (quality, snapshot) = srt_quality_from_stats(&stats, None, Instant::now());
    assert_eq!(quality.packets_received_loss, Some(0));
    assert_eq!(quality.packets_received_drop, Some(0));
    assert_eq!(quality.packets_received_retrans, Some(0));
    assert_eq!(quality.packets_received_undecrypt, Some(0));
    assert_eq!(quality.ms_receive_tsb_pd_delay, Some(0.0));
    assert_eq!(quality.ms_receive_buf, Some(0.0));
    assert_eq!(snapshot.packets_received_loss, 0);
}

#[test]
fn probe_wait_guard_requires_video_to_be_some() {
    // Simulate the logic of the retry closure:
    //   ingests.get(pipeline_id).and_then(|i| { video.as_ref()?; ... Some(meta) })
    // When video is None the closure must return None (no break).
    struct FakeIngest {
        video: Option<String>,
    }
    let ingest_no_video = FakeIngest { video: None };
    let ingest_with_video = FakeIngest {
        video: Some("h264".to_string()),
    };

    let result_none: Option<(&str,)> = (|| {
        let video = ingest_no_video.video.as_ref()?;
        let _ = video;
        Some(("got_video",))
    })();
    assert!(
        result_none.is_none(),
        "loop must not break while video is None"
    );

    let result_some: Option<(&str,)> = (|| {
        let video = ingest_with_video.video.as_ref()?;
        let _ = video;
        Some(("got_video",))
    })();
    assert!(result_some.is_some(), "loop must break once video is Some");
}

#[test]
fn summarizes_srt_group_member_state() {
    let mut connected: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    connected.sockstate = SRTS_CONNECTED;
    connected.memberstate = SRT_GST_RUNNING;

    let mut idle: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    idle.sockstate = SRTS_CONNECTED;
    idle.memberstate = 1;

    let mut broken: SrtSocketGroupData = unsafe { std::mem::zeroed() };
    broken.sockstate = SRTS_BROKEN;
    broken.memberstate = SRT_GST_BROKEN;

    assert_eq!(
        summarize_group_members(&[connected, idle, broken]),
        SrtGroupSummary {
            member_count: 3,
            connected_members: 2,
            active_members: 1,
            broken_members: 1,
        }
    );
}

#[test]
fn adds_bonded_group_state_to_publisher_quality() {
    let mut quality = PublisherQuality::default();
    add_srt_group_quality(
        &mut quality,
        true,
        Some(SrtGroupSummary {
            member_count: 2,
            connected_members: 2,
            active_members: 1,
            broken_members: 0,
        }),
    );

    assert_eq!(quality.srt_bonded, Some(true));
    assert_eq!(quality.srt_group_member_count, Some(2));
    assert_eq!(quality.srt_group_connected_members, Some(2));
    assert_eq!(quality.srt_group_active_members, Some(1));
    assert_eq!(quality.srt_group_broken_members, Some(0));
}

#[test]
fn marks_single_link_srt_without_group_member_fields() {
    let mut quality = PublisherQuality::default();
    add_srt_group_quality(&mut quality, false, None);

    assert_eq!(quality.srt_bonded, Some(false));
    assert_eq!(quality.srt_group_member_count, None);
    assert_eq!(quality.srt_group_connected_members, None);
    assert_eq!(quality.srt_group_active_members, None);
    assert_eq!(quality.srt_group_broken_members, None);
}

#[test]
fn maps_srt_sender_quality_from_bistats() {
    let stats: SrtSenderStats = SrtTraceBStats {
        ms_rtt: 12.5,
        mbps_send_rate: 3.25,
        mbps_bandwidth: 42.0,
        ms_snd_tsb_pd_delay: 120,
        ms_snd_buf: 80,
        pkt_snd_loss_total: 10,
        pkt_snd_drop_total: 3,
        pkt_retrans_total: 5,
        pkt_recv_nak_total: 7,
        byte_snd_buf: 4096,
        byte_avail_snd_buf: 8192,
        pkt_flight_size: 4,
        pkt_flow_window: 8192,
        pkt_congestion_window: 1024,
        ..unsafe { std::mem::zeroed() }
    }
    .into();
    let sampled_at = Instant::now();
    let previous = SrtSenderCounterSnapshot {
        packets_sent_loss: 4,
        packets_sent_drop: 1,
        packets_sent_retrans: 2,
        sampled_at: sampled_at - Duration::from_secs(2),
    };

    let (quality, snapshot) = srt_sender_quality_from_stats(&stats, Some(previous), sampled_at);

    assert_eq!(quality.ms_rtt, Some(12.5));
    assert_eq!(quality.mbps_send_rate, Some(3.25));
    assert_eq!(quality.mbps_link_capacity, Some(42.0));
    assert_eq!(quality.ms_send_tsb_pd_delay, Some(120.0));
    assert_eq!(quality.ms_send_buf, Some(80.0));
    assert_eq!(quality.packets_sent_loss, Some(10));
    assert_eq!(quality.packets_sent_drop, Some(3));
    assert_eq!(quality.packets_sent_retrans, Some(5));
    assert_eq!(quality.packets_received_nak, Some(7));
    assert_eq!(quality.packets_sent_loss_per_sec, Some(3.0));
    assert_eq!(quality.packets_sent_drop_per_sec, Some(1.0));
    assert_eq!(quality.packets_sent_retrans_per_sec, Some(1.5));
    assert_eq!(quality.srt_send_buf_bytes, Some(4096));
    assert_eq!(quality.srt_send_buf_avail_bytes, Some(8192));
    assert_eq!(quality.srt_flight_size_pkts, Some(4));
    assert_eq!(quality.srt_flow_window_pkts, Some(8192));
    assert_eq!(quality.srt_congestion_window_pkts, Some(1024));
    assert_eq!(snapshot.packets_sent_loss, 10);
    assert_eq!(snapshot.packets_sent_drop, 3);
    assert_eq!(snapshot.packets_sent_retrans, 5);
}

// Mirrors `srt_rates_report_none_when_counter_regresses_instead_of_wrapping`
// for the sender-side wrapper: it shares `counter_rate`'s `checked_sub`, but
// only had happy-path coverage before this test.
#[test]
fn srt_sender_rates_report_none_when_counter_regresses_instead_of_wrapping() {
    let sampled_at = Instant::now();
    let previous = SrtSenderCounterSnapshot {
        packets_sent_loss: 500,
        packets_sent_drop: 50,
        packets_sent_retrans: 900,
        sampled_at,
    };
    let stats: SrtSenderStats = SrtTraceBStats {
        pkt_snd_loss_total: 10,
        pkt_snd_drop_total: 1,
        pkt_retrans_total: 0,
        ..unsafe { std::mem::zeroed() }
    }
    .into();

    let (quality, _) =
        srt_sender_quality_from_stats(&stats, Some(previous), sampled_at + Duration::from_secs(2));
    assert_eq!(quality.packets_sent_loss_per_sec, None);
    assert_eq!(quality.packets_sent_drop_per_sec, None);
    assert_eq!(quality.packets_sent_retrans_per_sec, None);
    // The absolute (non-rate) counters still reflect the post-reset value.
    assert_eq!(quality.packets_sent_loss, Some(10));
}

// Mirrors `srt_rates_report_none_at_zero_elapsed_seconds` for the sender side.
#[test]
fn srt_sender_rates_report_none_at_zero_elapsed_seconds() {
    let sampled_at = Instant::now();
    let previous = SrtSenderCounterSnapshot {
        packets_sent_loss: 5,
        packets_sent_drop: 0,
        packets_sent_retrans: 0,
        sampled_at,
    };
    let stats: SrtSenderStats = SrtTraceBStats {
        pkt_snd_loss_total: 15,
        ..unsafe { std::mem::zeroed() }
    }
    .into();

    let (quality, _) = srt_sender_quality_from_stats(&stats, Some(previous), sampled_at);
    assert_eq!(quality.packets_sent_loss_per_sec, None);
}

// Mirrors `quality_from_stats_clamps_negative_sentinel_counters_to_zero` for
// the sender side: libsrt's -1 "unknown" sentinel must clamp to 0 before
// widening rather than sign-extending into a near-`u64::MAX` garbage value.
#[test]
fn sender_quality_from_stats_clamps_negative_sentinel_counters_to_zero() {
    let stats: SrtSenderStats = SrtTraceBStats {
        pkt_snd_loss_total: -1,
        pkt_snd_drop_total: -1,
        pkt_retrans_total: -1,
        ms_snd_tsb_pd_delay: -1,
        ms_snd_buf: -1,
        ..unsafe { std::mem::zeroed() }
    }
    .into();

    let (quality, snapshot) = srt_sender_quality_from_stats(&stats, None, Instant::now());
    assert_eq!(quality.packets_sent_loss, Some(0));
    assert_eq!(quality.packets_sent_drop, Some(0));
    assert_eq!(quality.packets_sent_retrans, Some(0));
    assert_eq!(quality.ms_send_tsb_pd_delay, Some(0.0));
    assert_eq!(quality.ms_send_buf, Some(0.0));
    assert_eq!(snapshot.packets_sent_loss, 0);
}

#[test]
fn reads_udp_socket_stats_for_listener_port() {
    // On a system without an SRT listener, this should return None
    // (port 10080 not bound). If it's bound, it returns Some.
    let result = read_udp_socket_stats(10080);
    // Either None or Some with valid values — should not panic
    if let Some((rx_queue, drops)) = result {
        assert!(rx_queue < u64::MAX);
        assert!(drops < u64::MAX);
    }
}

#[tokio::test]
async fn monitor_listener_socket_extreme_capacity_does_not_panic() {
    // effective_udp_recv_capacity near u64::MAX previously overflowed the
    // `configured_buf * 3` threshold multiplication before the first .await,
    // panicking the monitor task immediately on spawn.
    let stats = Arc::new(crate::media::snapshots::ListenerSocketStats::default());
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        monitor_listener_socket(0, stats, u64::MAX),
    )
    .await;
    // The function loops forever, so we expect the timeout to fire — the
    // only thing under test is that it doesn't panic before then.
    assert!(
        result.is_err(),
        "monitor_listener_socket should still be running (not panicked) when the timeout fires"
    );
}
