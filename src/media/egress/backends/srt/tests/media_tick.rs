//! Regression test for `on_media_tick`'s connect-completion scheduling —
//! the SRT counterpart of `rtmp_shard_media_tick_tests.rs`. See the doc
//! comment on `EgressShardBackend::on_media_tick` (`src/media/egress/shard.rs`)
//! for the full story: nothing previously told the shard runtime to give a
//! freshly-connected leaf its first readiness check, so it sat registered
//! but unvisited until an unrelated `FeedWake` happened to arrive.

use super::super::*;
use super::support::{FakeReadinessPoller, FakeSocketConfigurator, FakeSocketConnector, feed};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use bytes::Bytes;
use std::time::Duration;

fn output_spec(id: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol: ProtocolSpec::Srt {
            url: "srt://127.0.0.1:9000?streamid=publish%3Akey".to_string(),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[test]
fn on_media_tick_schedules_ready_work_when_a_connect_completes() {
    let (sender, queue) = srt_resolve_completion_queue(4);
    let mut backend = SrtShardBackend::with_runtime_components(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
        FakeSocketConnector::returning(42),
        queue,
    );
    let output_id = OutputId::new("media-tick-leaf");
    backend.on_command(EgressCommand::Add(output_spec("media-tick-leaf", 1)));
    sender
        .send(SrtResolvedConnect {
            output_id: output_id.clone(),
            generation: 1,
            peer_addrs: vec!["127.0.0.1:9000".parse().unwrap()],
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
}

#[test]
fn on_media_tick_is_a_no_op_when_nothing_resolved() {
    let (_sender, queue) = srt_resolve_completion_queue(4);
    let mut backend = SrtShardBackend::with_runtime_components(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
        FakeSocketConnector::returning(42),
        queue,
    );

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::Continue,
        "an idle tick with no resolved connects must not schedule ready work"
    );
}
