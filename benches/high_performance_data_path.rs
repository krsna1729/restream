use criterion::{Criterion, criterion_group, criterion_main};

#[path = "high_performance_data_path/burst.rs"]
mod burst;
#[path = "high_performance_data_path/mpegts.rs"]
mod mpegts;
#[path = "high_performance_data_path/queue_segments.rs"]
mod queue_segments;
#[path = "high_performance_data_path/registry.rs"]
mod registry;
#[path = "high_performance_data_path/ring_fanout.rs"]
mod ring_fanout;
#[path = "high_performance_data_path/support.rs"]
mod support;

fn benches(c: &mut Criterion) {
    support::print_layout_baseline();
    registry::register_hot_handles(c);
    ring_fanout::register(c);
    queue_segments::register(c);
    mpegts::register(c);
    burst::register(c);
    registry::register_keyframe(c);
}

criterion_group!(data_path_benches, benches);
criterion_main!(data_path_benches);
