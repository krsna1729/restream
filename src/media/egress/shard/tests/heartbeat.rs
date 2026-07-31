use super::super::*;
use super::support::snapshot;
use crate::media::egress::command::ShardId;
use std::time::{Duration, Instant};

#[test]
fn heartbeat_classifies_snapshot_health_states() {
    let now = Instant::now();
    let base = EgressShardSnapshot {
        loop_iterations: 7,
        commands_processed: 3,
        timers_processed: 2,
        pending_timers: 1,
        media_ticks: 5,
        last_progress_at: Some(now - Duration::from_millis(10)),
        ..snapshot(ShardId::new(0))
    };

    let healthy =
        EgressShardHeartbeat::from_snapshot(base.clone(), now, Duration::from_millis(100));
    let stalled = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            last_progress_at: Some(now - Duration::from_secs(5)),
            ..base.clone()
        },
        now,
        Duration::from_millis(100),
    );
    let stopped = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            stopped: true,
            ..base.clone()
        },
        now,
        Duration::from_millis(100),
    );
    let panicked = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            panicked: true,
            stopped: true,
            ..base
        },
        now,
        Duration::from_millis(100),
    );

    assert_eq!(healthy.state, EgressShardHealth::Healthy);
    assert_eq!(healthy.loop_iterations, 7);
    assert_eq!(healthy.media_ticks, 5);
    assert_eq!(healthy.progress_age, Some(Duration::from_millis(10)));
    assert_eq!(stalled.state, EgressShardHealth::Stalled);
    assert_eq!(stopped.state, EgressShardHealth::Stopped);
    assert_eq!(panicked.state, EgressShardHealth::Panicked);
}

#[test]
fn heartbeat_treats_missing_progress_as_stalled() {
    let now = Instant::now();
    let heartbeat = EgressShardHeartbeat::from_snapshot(
        EgressShardSnapshot {
            last_progress_at: None,
            ..snapshot(ShardId::new(0))
        },
        now,
        Duration::from_millis(100),
    );

    assert_eq!(heartbeat.state, EgressShardHealth::Stalled);
    assert_eq!(heartbeat.progress_age, None);
}
