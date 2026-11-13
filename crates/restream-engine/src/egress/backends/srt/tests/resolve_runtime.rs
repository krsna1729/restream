use super::super::resolve_runtime::{
    ResolvingSrtShardBackend, ResolvingSrtShardBackendDefault, resolving_srt_shard_backend,
};
use super::super::*;
use super::support::*;
use crate::egress::command::ShardId;
use crate::egress::shard::EgressShardBackend;
use crate::resolver::Resolver;
use crate::resolver::fake::FakeDns;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

fn resolving_backend(resolver: Arc<Resolver>) -> (ResolvingSrtShardBackendDefault, TestFeed) {
    let feed = TestFeed::new();
    let backend = resolving_srt_shard_backend(
        feed.reader(),
        budget(),
        Duration::from_millis(400),
        64,
        settings(),
        resolver,
    )
    .expect("shard runtime");
    (backend, feed)
}

fn tick_until(
    backend: &mut ResolvingSrtShardBackendDefault,
    within: Duration,
    mut done: impl FnMut(&SrtShardBackend) -> bool,
) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        backend.on_media_tick();
        if done(backend.inner_backend()) {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    false
}

fn spec_resolving_within(
    id: &str,
    generation: u64,
    url: &str,
    resolve_timeout: Duration,
) -> (OutputSpec, Arc<AtomicBool>) {
    let (mut spec, flag) = srt_spec_with_flag(id, generation, url);
    spec.policy.resolve_timeout = resolve_timeout;
    (spec, flag)
}

/// The full DNS -> Owner path: numeric hosts need no lookup, and the
/// completion attaches the bond to one live leaf.
#[test]
fn resolving_srt_backend_resolves_and_completes_add() {
    let (mut backend, _feed) = resolving_backend(crate::resolver::shared());
    let effect = backend.on_command(EgressCommand::Add(srt_spec(
        "out-a",
        7,
        "srt://127.0.0.1:9000?streamid=publish%3Akey&bond=127.0.0.2:9001",
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert!(tick_until(&mut backend, Duration::from_secs(1), |inner| {
        inner.output_sockets.contains_key(&OutputId::new("out-a"))
    }));
    assert_eq!(
        backend.inner_backend().callers.len(),
        1,
        "one logical caller for the whole bond"
    );
}

#[test]
fn resolving_srt_backend_does_not_resolve_a_non_srt_add() {
    let dns = FakeDns::new();
    let (mut backend, _feed) = resolving_backend(dns.resolver.clone());
    let mut spec = srt_spec("out-a", 7, "unused");
    spec.protocol = ProtocolSpec::Sink;
    assert_eq!(
        backend.on_command(EgressCommand::Add(spec)),
        EgressShardCommandEffect::Continue
    );
    backend.on_media_tick();
    assert!(backend.inner_backend().pending_connects.is_empty());
    assert_eq!(dns.calls(), 0);
}

#[test]
fn resolving_srt_backend_retires_failed_dns_connect() {
    let dns = FakeDns::new();
    let (mut backend, _feed) = resolving_backend(dns.resolver.clone());
    let (spec, flag) =
        srt_spec_with_flag("out-a", 7, "srt://fail.test:9000?streamid=publish%3Akey");
    backend.on_command(EgressCommand::Add(spec));
    assert!(
        tick_until(&mut backend, Duration::from_secs(1), |inner| {
            inner.pending_connects.is_empty()
        }),
        "failed DNS must not leave a pending connect resident"
    );
    assert!(flag.load(Ordering::Relaxed), "and is reported terminated");
}

/// One black-holed host on a shard leaves a sibling output connecting at
/// once. A bond with one hung leg fails as a whole at its resolve deadline
/// (its egress task retries it).
#[test]
fn a_black_holed_destination_never_delays_a_sibling_on_the_shard() {
    let dns = FakeDns::new();
    let (mut backend, _feed) = resolving_backend(dns.resolver.clone());
    let timeout = Duration::from_millis(300);
    let (hung, hung_failed) = spec_resolving_within(
        "hung",
        1,
        "srt://ok.test:9000?streamid=publish%3Akey&bond=hung.test:9001",
        timeout,
    );
    let started = Instant::now();
    backend.on_command(EgressCommand::Add(hung));
    dns.wait_for_calls(2);

    let (sibling, _) = spec_resolving_within(
        "sibling",
        1,
        "srt://sibling.test:9000?streamid=publish%3Akey",
        Duration::from_secs(5),
    );
    backend.on_command(EgressCommand::Add(sibling));
    assert!(
        tick_until(&mut backend, Duration::from_secs(1), |inner| {
            inner.output_sockets.contains_key(&OutputId::new("sibling"))
        }),
        "the sibling connected while the hung lookup runs"
    );
    let inner = backend.inner_backend();
    assert!(inner.pending_connects.contains_key(&OutputId::new("hung")));

    assert!(
        tick_until(&mut backend, Duration::from_secs(2), |inner| {
            inner.pending_connects.is_empty()
        }),
        "the hung bond fails at its deadline"
    );
    assert!(hung_failed.load(Ordering::Relaxed));
    assert!(started.elapsed() >= timeout);
    assert!(
        !backend
            .inner_backend()
            .output_sockets
            .contains_key(&OutputId::new("hung"))
    );
}

/// More outputs than the resolver's queue holds: every one reaches a
/// terminal state (refused at once, or timed out at its deadline), none is
/// left pending holding a leaf slot.
#[test]
fn every_output_past_the_resolver_queue_gets_a_terminal_state() {
    let dns = FakeDns::with_pool(1, 4);
    let (mut backend, _feed) = resolving_backend(dns.resolver.clone());
    let timeout = Duration::from_millis(300);
    let (hung, hung_flag) = spec_resolving_within(
        "hung",
        1,
        "srt://hung.test:9000?streamid=publish%3Akey",
        timeout,
    );
    backend.on_command(EgressCommand::Add(hung));
    dns.wait_for_calls(1);
    let mut flags = vec![hung_flag];
    for index in 0..20 {
        let (spec, flag) = spec_resolving_within(
            &format!("out-{index}"),
            1,
            &format!("srt://name-{index}.test:9000?streamid=publish%3Akey"),
            timeout,
        );
        backend.on_command(EgressCommand::Add(spec));
        flags.push(flag);
    }
    backend.on_media_tick();
    assert_eq!(
        dns.resolver.stats().overflows,
        16,
        "four names fit the queue"
    );
    assert_eq!(
        flags
            .iter()
            .filter(|flag| flag.load(Ordering::Relaxed))
            .count(),
        16,
        "the refused outputs failed at once"
    );

    assert!(
        tick_until(&mut backend, Duration::from_secs(2), |inner| {
            inner.pending_connects.is_empty()
        }),
        "every output was answered"
    );
    assert!(flags.iter().all(|flag| flag.load(Ordering::Relaxed)));
    let inner = backend.inner_backend();
    assert!(inner.output_sockets.is_empty() && inner.leaves.is_empty());
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
        metrics.stale_completions = 13;
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
    let (completion_sender, _completion_queue) = srt_resolve_completion_queue();
    let mut backend = ResolvingSrtShardBackend::new(
        ForwardingProbe,
        crate::resolver::shared(),
        completion_sender,
    );
    let mut metrics = ShardMetrics::new(ShardId::new(0));

    assert_eq!(backend.resync_count(), 7);
    assert_eq!(backend.budget_exhaustion_count(), 11);
    backend.observe_metrics(&mut metrics);
    assert_eq!(metrics.stale_completions, 13);

    let (_tx, rx) = flume::bounded(1);
    assert!(matches!(
        backend.wait_idle(&rx, Duration::from_secs(30)),
        EgressShardIdleWake::BackendActivity
    ));
}
