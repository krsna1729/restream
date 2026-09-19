use super::super::resolve_runtime::{ResolvingSrtShardBackend, SrtResolveWorkerSet};
use super::super::*;
use super::support::*;
use crate::media::egress::command::ShardId;
use crate::media::egress::shard::EgressShardBackend;
use std::thread;
use std::time::Duration;

fn resolving_backend() -> (ResolvingSrtShardBackend<SrtShardBackend>, TestFeed) {
    let feed = TestFeed::new();
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let inner = SrtShardBackend::with_runtime_components(
        feed.reader(),
        budget(),
        completion_queue,
        SrtOwners::new(settings()).expect("shard runtime"),
    );
    (
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender)),
        feed,
    )
}

/// The full DNS -> Owner path through the resolver worker: a numeric host
/// resolves off-thread, then the completion is attached to a live leaf.
#[test]
fn resolving_srt_backend_resolves_off_thread_and_completes_add() {
    let (mut backend, _feed) = resolving_backend();
    let effect = backend.on_command(EgressCommand::Add(srt_spec(
        "out-a",
        7,
        "srt://127.0.0.1:9000?streamid=publish%3Akey&bond=127.0.0.2:9001",
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    for _ in 0..500 {
        backend.on_media_tick();
        if backend
            .inner_backend()
            .output_sockets
            .contains_key(&OutputId::new("out-a"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let inner = backend.inner_backend();
    assert!(inner.output_sockets.contains_key(&OutputId::new("out-a")));
    assert_eq!(
        inner.callers.len(),
        1,
        "one logical caller for the whole bond"
    );
}

#[test]
fn resolving_srt_backend_does_not_resolve_a_non_srt_add() {
    let (mut backend, _feed) = resolving_backend();
    let mut spec = srt_spec("out-a", 7, "unused");
    spec.protocol = ProtocolSpec::Sink;
    assert_eq!(
        backend.on_command(EgressCommand::Add(spec)),
        EgressShardCommandEffect::Continue
    );
    backend.on_media_tick();
    assert!(backend.inner_backend().pending_connects.is_empty());
}

#[test]
fn resolving_srt_backend_retires_failed_dns_connect() {
    let (mut backend, _feed) = resolving_backend();
    let (spec, flag) = srt_spec_with_flag(
        "out-a",
        7,
        "srt://256.256.256.256:9000?streamid=publish%3Akey",
    );
    backend.on_command(EgressCommand::Add(spec));
    for _ in 0..500 {
        backend.on_media_tick();
        if backend.inner_backend().pending_connects.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        backend.inner_backend().pending_connects.is_empty(),
        "failed DNS must not leave a pending connect resident"
    );
    assert!(
        flag.load(std::sync::atomic::Ordering::Relaxed),
        "and is reported terminated"
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

    fn wait_idle(
        &mut self,
        _commands: &flume::Receiver<EgressCommand>,
        _max_wait: Duration,
    ) -> EgressShardIdleWake {
        EgressShardIdleWake::BackendActivity
    }
}

/// The decorator must forward everything the shard reads from a backend,
/// including the idle wait (the default channel wait would bypass the wrapped
/// backend's Compio park).
#[test]
fn resolving_srt_backend_forwards_metrics_and_the_idle_wait() {
    let (completion_sender, _completion_queue) = srt_resolve_completion_queue(1);
    let mut backend =
        ResolvingSrtShardBackend::new(ForwardingProbe, SrtResolveWorkerSet::new(completion_sender));
    let mut metrics = ShardMetrics::new(ShardId::new(0));

    assert_eq!(backend.resync_count(), 7);
    assert_eq!(backend.budget_exhaustion_count(), 11);
    backend.observe_metrics(&mut metrics);
    assert_eq!(metrics.cq_overflows, 13);

    let (_tx, rx) = flume::bounded(1);
    assert!(matches!(
        backend.wait_idle(&rx, Duration::from_secs(30)),
        EgressShardIdleWake::BackendActivity
    ));
}
