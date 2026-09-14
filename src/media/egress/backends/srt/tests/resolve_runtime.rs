use super::super::resolve_runtime::{ResolvingSrtShardBackend, SrtResolveWorkerSet};
use super::super::*;
use super::support::feed;
use crate::media::egress::command::ShardId;
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use bytes::Bytes;
use std::thread;
use std::time::Duration;

fn output_spec(id: &str, generation: u64, protocol: ProtocolSpec) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[test]
fn resolving_srt_backend_spawns_resolver_and_completes_add() {
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let inner = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        completion_queue,
    );
    let mut backend =
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender));

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://127.0.0.1:9000?streamid=publish%3Akey&bond=127.0.0.2:9001".to_string(),
        },
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    for _ in 0..50 {
        backend.on_media_tick();
        if backend
            .inner_backend()
            .output_sockets
            .contains_key(&OutputId::new("out-a"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        backend
            .inner_backend()
            .output_sockets
            .contains_key(&OutputId::new("out-a"))
    );
    assert_eq!(backend.worker_count(), 0);
}

#[test]
fn resolving_srt_backend_does_not_spawn_for_non_srt_add() {
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let inner = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        completion_queue,
    );
    let mut backend =
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender));

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Sink,
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    backend.on_media_tick();
    assert_eq!(backend.worker_count(), 0);
}

#[test]
fn resolving_srt_backend_retires_failed_dns_connect() {
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let inner = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        completion_queue,
    );
    let mut backend =
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender));

    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://256.256.256.256:9000?streamid=publish%3Akey".to_string(),
        },
    )));
    for _ in 0..50 {
        backend.on_media_tick();
        if backend
            .inner_backend()
            .pending_connect(&OutputId::new("out-a"))
            .is_none()
        {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        backend
            .inner_backend()
            .pending_connect(&OutputId::new("out-a"))
            .is_none(),
        "failed DNS must not leave a pending connect resident"
    );
}

struct ForwardingProbe;

impl EgressShardBackend for ForwardingProbe {
    fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn resync_count(&self) -> u64 {
        7
    }

    fn budget_exhaustion_count(&self) -> u64 {
        11
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        metrics.cq_overflows = 13;
    }
}

#[test]
fn resolving_srt_backend_forwards_metrics() {
    let (completion_sender, _completion_queue) = srt_resolve_completion_queue(1);
    let backend =
        ResolvingSrtShardBackend::new(ForwardingProbe, SrtResolveWorkerSet::new(completion_sender));
    let mut metrics = ShardMetrics::new(ShardId::new(0));

    assert_eq!(backend.resync_count(), 7);
    assert_eq!(backend.budget_exhaustion_count(), 11);
    backend.observe_metrics(&mut metrics);
    assert_eq!(metrics.cq_overflows, 13);
}
