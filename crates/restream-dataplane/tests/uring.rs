use restream_dataplane::{Dataplane, DataplaneHandle, OutputRuntimeSpec, ShardConfig};

#[test]
fn owner_thread_services_only_woken_sinks() {
    let dataplane = match Dataplane::spawn(ShardConfig {
        max_leaves: 64,
        ready_capacity: 64,
        ..ShardConfig::default()
    }) {
        Ok(dataplane) => dataplane,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(error) => panic!("io_uring dataplane unavailable: {error}"),
    };

    dataplane.add_sink(7).unwrap();
    dataplane.add_sink(8).unwrap();
    assert!(dataplane.wake_sink(8).unwrap());
    let snapshot = (0..100)
        .map(|_| dataplane.snapshot().unwrap())
        .find(|snapshot| {
            snapshot
                .sinks
                .iter()
                .find(|sink| sink.id == 8)
                .is_some_and(|sink| sink.visits == 1)
        })
        .expect("owner thread should service the woken sink");
    assert_eq!(snapshot.active_leaves, 2);
    assert_eq!(
        snapshot
            .sinks
            .iter()
            .find(|sink| sink.id == 7)
            .unwrap()
            .visits,
        0
    );
    assert_eq!(
        snapshot
            .sinks
            .iter()
            .find(|sink| sink.id == 8)
            .unwrap()
            .visits,
        1
    );
    assert!(snapshot.metrics.ready_visits >= 1);
    dataplane.shutdown().unwrap();
}

#[test]
fn multi_shard_handle_keeps_output_placement_stable() {
    let dataplane = match DataplaneHandle::spawn(
        ShardConfig {
            max_leaves: 64,
            ready_capacity: 64,
            ..ShardConfig::default()
        },
        2,
    ) {
        Ok(dataplane) => dataplane,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(error) => panic!("io_uring dataplane unavailable: {error}"),
    };

    let first = dataplane
        .add_output_spec(OutputRuntimeSpec {
            id: 7,
            generation: 1,
        })
        .unwrap();
    let second = dataplane.add_output(8).unwrap();
    assert_eq!(first.shard, 1);
    assert_eq!(second.shard, 0);
    assert!(dataplane.update_output(7, 2).unwrap());
    assert!(!dataplane.update_output(7, 1).unwrap());
    assert!(dataplane.wake_output(7).unwrap());
    let snapshot = (0..100)
        .map(|_| dataplane.snapshot().unwrap())
        .find(|snapshot| {
            snapshot.shards[first.shard]
                .sinks
                .iter()
                .any(|sink| sink.id == 7 && sink.generation == 2 && sink.visits == 1)
        })
        .expect("the selected owner shard should service the woken output");
    assert_eq!(
        snapshot
            .shards
            .iter()
            .map(|shard| shard.active_leaves)
            .sum::<usize>(),
        2
    );
    assert_eq!(snapshot.shards[first.shard].metrics.ready_visits, 1);
    assert_eq!(snapshot.shards[second.shard].metrics.ready_visits, 0);
    dataplane.shutdown().unwrap();
}
