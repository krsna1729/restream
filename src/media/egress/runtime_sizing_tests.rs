use super::tests::{Probe, group, output_spec};
use super::*;
use crate::media::egress::metrics::ShardService;

fn runtime(shards: u32) -> EgressFabricRuntime {
    let probes: Vec<_> = (0..shards).map(|_| Probe::default()).collect();
    let mut runtime = EgressFabricRuntime::new(
        EgressManagerConfig::new(shards, 16).unwrap(),
        group(shards, &probes),
    )
    .unwrap()
    .adaptive(8);
    for i in 0..4 {
        runtime
            .dispatch(EgressCommand::Add(output_spec(&format!("sample-{i}"))))
            .unwrap();
    }
    runtime
}

fn frame(
    runtime: &EgressFabricRuntime,
    start: Instant,
    step: u64,
    busy: f64,
    under_floor: bool,
) -> Vec<EgressShardSnapshot> {
    let mut frame = runtime.group.snapshots();
    for (i, snapshot) in frame.iter_mut().enumerate() {
        let rated = if i == 0 { 4 } else { 0 };
        let at = start + Duration::from_secs(step * 5);
        let ratio = if under_floor { 0.2 } else { 1.0 };
        snapshot.metrics.thread_cpu_at = Some(at);
        snapshot.metrics.thread_cpu_ns = (step as f64 * 5.0 * busy * 1e9) as u64;
        snapshot.metrics.service = ShardService {
            visited: rated,
            rated,
            under_floor: if under_floor { rated } else { 0 },
            offered_bps: f64::from(rated) * 4.8e6,
            delivered_bps: f64::from(rated) * 4.8e6 * ratio,
            sum_ratio: f64::from(rated) * ratio,
            sum_sq_ratio: f64::from(rated) * ratio * ratio,
            completed_at: Some(at),
            ..Default::default()
        };
    }
    frame
}

#[test]
fn missing_unchanged_and_reset_cpu_samples_are_not_observations() {
    for invalid in 0..3 {
        let start = Instant::now();
        let mut runtime = runtime(1);
        runtime.sizer.resized(start);
        assert!(!runtime.observe_samples(&frame(&runtime, start, 0, 0.7, true)));
        assert!(runtime.observe_samples(&frame(&runtime, start, 1, 0.7, true)));
        let mut broken = frame(&runtime, start, 2, 0.7, true);
        match invalid {
            0 => broken[0].metrics.thread_cpu_at = None,
            1 => broken[0].metrics.thread_cpu_at = Some(start + Duration::from_secs(5)),
            _ => broken[0].metrics.thread_cpu_ns = 0,
        }
        assert!(!runtime.observe_samples(&broken));
        runtime.shutdown();
    }
}

#[test]
fn invalid_cpu_sample_interrupts_the_sixty_window_shrink_streak() {
    let start = Instant::now();
    let mut runtime = runtime(3);
    runtime.sizer.resized(start);
    runtime.observe_samples(&frame(&runtime, start, 0, 0.03, false));
    for step in 1..=59 {
        assert!(runtime.observe_samples(&frame(&runtime, start, step, 0.03, false)));
    }
    let mut invalid = frame(&runtime, start, 60, 0.03, false);
    invalid[1].metrics.thread_cpu_at = None;
    assert!(!runtime.observe_samples(&invalid));
    assert!(runtime.observe_samples(&frame(&runtime, start, 61, 0.03, false)));
    assert_eq!(
        runtime
            .sizer
            .shrink_target(start + Duration::from_secs(305), 4, 3, 64),
        3
    );
    runtime.shutdown();
}

#[test]
fn a_completed_service_rotation_is_consumed_only_once() {
    let start = Instant::now();
    let mut runtime = runtime(1);
    runtime.observe_samples(&frame(&runtime, start, 0, 0.7, false));
    let complete = frame(&runtime, start, 1, 0.7, false);
    assert!(runtime.observe_samples(&complete));
    let mut unchanged_service = frame(&runtime, start, 2, 0.7, false);
    unchanged_service[0].metrics.service = complete[0].metrics.service;
    assert!(!runtime.observe_samples(&unchanged_service));
    runtime.shutdown();
}

#[test]
fn replacement_ring_resets_forecast_counter_even_if_new_counter_is_larger() {
    let mut runtime = runtime(1);
    let old = RingBuffer::new(8);
    let new = RingBuffer::new(8);
    runtime.sizer.forecast_feed(4.8e6);
    runtime.forecast_feed(&old);
    runtime.feed_mark.as_mut().unwrap().2 -= Duration::from_millis(500);
    new.push(crate::media::packet::MediaPacket {
        media_type: crate::media::packet::MediaType::Video,
        format: crate::media::packet::PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from(vec![0; 1_000_000]),
    });
    runtime.forecast_feed(&new);
    assert_eq!(
        runtime.sizer.recommend(64, 64, 8),
        1,
        "the replacement's retained payload must not look like a sudden bitrate spike"
    );
    runtime.shutdown();
}
