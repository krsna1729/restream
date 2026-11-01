use super::super::tests::{Probe, group, output_spec};
use super::super::*;
use crate::egress::metrics::ShardService;

/// Default leaf tables (4,096 per shard): these tests refuse on busy time.
fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5)).unwrap()
}

fn runtime(shards: u32) -> EgressFabricRuntime {
    let probes: Vec<_> = (0..shards).map(|_| Probe::default()).collect();
    let mut runtime = EgressFabricRuntime::new(
        EgressManagerConfig::new(shards, 16).unwrap(),
        group(shards, &probes),
        8,
    )
    .unwrap();
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
    new.push(restream_core::MediaPacket {
        media_type: restream_core::MediaType::Video,
        format: restream_core::PayloadFormat::Raw,
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

#[test]
fn a_burst_of_adds_on_a_new_runtime_is_priced_at_the_feed_rate() {
    let profile = crate::config::EgressShardProfile::SrtOutputCount;
    let probes = [Probe::default()];
    let mut runtime = super::super::tests::roomy_runtime(&probes).0;
    let ring = RingBuffer::new(8);
    // 2 MB over 500 ms of media time: 32 Mbit/s, well over the 4.8 Mbit/s
    // reference rate, all published before the runtime first looks.
    for dts in [0, 500] {
        ring.push(restream_core::MediaPacket {
            media_type: restream_core::MediaType::Video,
            format: restream_core::PayloadFormat::Raw,
            is_keyframe: dts == 0,
            track_index: 0,
            pts: dts,
            dts,
            payload: bytes::Bytes::from(vec![0; 1_000_000]),
        });
    }
    // The runtime's first look at the ring, as on its first Add.
    runtime.forecast_feed(&ring);
    let id = output_spec("burst").id;
    assert!(
        runtime.admit_add(profile, shard_config(), &id).is_ok(),
        "the first output is far below any ceiling"
    );
    for i in 0..64 {
        runtime
            .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
            .unwrap();
    }
    // 64 outputs at the reference rate fit (0.5 busy); at 32 Mbit/s they do
    // not.
    assert!(runtime.admit_add(profile, shard_config(), &id).is_err());
    runtime.shutdown();
}

#[test]
fn an_add_grows_the_pool_for_the_feed_rate_not_the_output_count() {
    let profile = crate::config::EgressShardProfile::OutputCount;
    let host = crate::host_capacity::HostCapacity { cpus: 8 };
    let probes = [Probe::default()];
    let (mut runtime, config) = super::super::tests::roomy_runtime(&probes);
    for i in 0..19 {
        runtime
            .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
            .unwrap();
    }
    let grown = Probe::default();
    let add = |runtime: &mut EgressFabricRuntime| {
        runtime
            .rescale(profile, host, ResizeReason::Add, config, |_| {
                let probe = grown.clone();
                move || Ok::<_, String>(super::super::tests::ProbeBackend { probe })
            })
            .unwrap()
    };
    // With the pending Add, 20 outputs at the 4.8 Mbit/s reference rate
    // project 7.8% busy on one shard, under the 50% target: no growth.
    assert!(add(&mut runtime).is_empty());
    // At 8x the rate they project 62.5%: two shards. The output-count law
    // (20 of 128 per shard) would still say one.
    runtime.sizer.forecast_feed(4.8e6 * 8.0);
    assert_eq!(add(&mut runtime), vec![ShardId::new(1)]);
    runtime.shutdown();
}

#[test]
fn admission_holds_every_shard_under_the_ceiling() {
    let profile = crate::config::EgressShardProfile::SrtOutputCount;
    let probes = [Probe::default(), Probe::default()];
    let mut runtime = EgressFabricRuntime::new(
        EgressManagerConfig::new(2, 1024)
            .unwrap()
            .with_placement_policy(crate::egress::manager::PlacementPolicy::LeastLoaded),
        group(2, &probes),
        u32::MAX,
    )
    .unwrap();
    // An 8× reference feed: one shard holds 9 outputs under the ceiling, so
    // two hold 18; the pool average alone would also admit a 19th.
    runtime.sizer.forecast_feed(4.8e6 * 8.0);
    for i in 0..18 {
        let spec = output_spec(&format!("out-{i}"));
        assert_eq!(
            runtime.admit_add(profile, shard_config(), &spec.id),
            Ok(()),
            "output {i}"
        );
        runtime.dispatch(EgressCommand::Add(spec)).unwrap();
    }
    for shard in 0..2 {
        assert_eq!(
            runtime.manager.desired_count_for_shard(ShardId::new(shard)),
            9
        );
    }
    let refused = runtime
        .admit_add(profile, shard_config(), &output_spec("out-18").id)
        .unwrap_err();
    assert!(
        matches!(refused, Saturated::Busy { busy_pct, shards: 2 } if busy_pct > 60),
        "{refused:?}"
    );
    runtime.shutdown();
}

/// The benchmark admission switch exists only in `srt-shard-bench` builds:
/// there it admits past the ceiling; a default build refuses whatever the
/// environment says.
#[test]
fn the_admission_bypass_is_benchmark_only() {
    let profile = crate::config::EgressShardProfile::SrtOutputCount;
    let probes = [Probe::default()];
    let mut runtime = EgressFabricRuntime::new(
        EgressManagerConfig::new(1, 1024).unwrap(),
        group(1, &probes),
        u32::MAX,
    )
    .unwrap();
    runtime.sizer.forecast_feed(4.8e6 * 8.0);
    for i in 0..9 {
        runtime
            .dispatch(EgressCommand::Add(output_spec(&format!("out-{i}"))))
            .unwrap();
    }
    let next = output_spec("out-9").id;
    assert!(
        runtime.admit_add(profile, shard_config(), &next).is_err(),
        "past the ceiling"
    );
    crate::config::env::with_env_vars(&[("RESTREAM_BENCH_SRT_ADMISSION", "0")], || {
        assert_eq!(
            runtime.admit_add(profile, shard_config(), &next).is_ok(),
            cfg!(feature = "srt-shard-bench")
        );
    });
    runtime.shutdown();
}
