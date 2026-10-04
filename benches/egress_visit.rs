//! One scheduler visit of an egress leaf (`EngineVisit::run`): generation
//! check, priming, the engine's advance, and the progress-to-decision
//! mapping. The engine is the scripted test driver yielding immediately, so
//! the visit's own fixed cost (including the per-visit panic boundary) is
//! what is measured, not protocol work.

use criterion::{Criterion, criterion_group, criterion_main};
use restream::media::egress::backend::Readiness;
use restream::media::egress::command::{FeedId, OutputId};
use restream::media::egress::leaf::LeafCommon;
use restream::media::egress::policy::{LeafLimits, WorkBudget};
use restream::media::egress::test_driver::{FakeEngine, FakeFeed, FakeTransport};
use restream::media::egress::visit::EngineVisit;
use std::hint::black_box;
use std::time::Duration;

fn bench_visit(c: &mut Criterion) {
    let feed = FakeFeed::new();
    let mut common = LeafCommon::new(
        OutputId::new("out"),
        1,
        FeedId::new("feed"),
        LeafLimits::default(),
    );
    let mut engine = FakeEngine::new(Vec::new());
    let mut transport = FakeTransport::default();
    let budget = WorkBudget::new(4, 4096, Duration::from_millis(1));
    c.bench_function("egress_visit/yielding_engine", |b| {
        b.iter(|| {
            black_box(
                EngineVisit {
                    generation: 1,
                    common: &mut common,
                    engine: &mut engine,
                    transport: &mut transport,
                    readiness: Readiness::WRITABLE,
                    feed: &feed,
                    budget,
                }
                .run(),
            );
        });
    });
}

criterion_group!(benches, bench_visit);
criterion_main!(benches);
