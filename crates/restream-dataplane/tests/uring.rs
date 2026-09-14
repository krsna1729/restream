use restream_dataplane::{Dataplane, DataplaneHandle, OutputRuntimeSpec, ShardConfig};
use std::time::{Duration, Instant};

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

    let _first = dataplane.add_sink(7).unwrap();
    let second = dataplane.add_sink(8).unwrap();
    assert!(
        dataplane
            .wake_sink(restream_dataplane::OutputHandle {
                shard: 0,
                slot: second.0,
                generation: second.1,
            })
            .unwrap()
    );
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
fn owner_thread_wakes_the_earliest_due_deadline() {
    let dataplane = match Dataplane::spawn(ShardConfig {
        max_leaves: 8,
        ready_capacity: 8,
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

    let parts = dataplane.add_sink(9).unwrap();
    let handle = restream_dataplane::OutputHandle {
        shard: 0,
        slot: parts.0,
        generation: parts.1,
    };
    dataplane
        .set_deadline(handle, Instant::now() + Duration::from_millis(200))
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    dataplane
        .set_deadline(handle, Instant::now() + Duration::from_millis(20))
        .unwrap();

    std::thread::sleep(Duration::from_millis(40));
    let snapshot = (0..100)
        .map(|_| dataplane.snapshot().unwrap())
        .find(|snapshot| snapshot.metrics.timers_processed >= 1)
        .expect("the io_uring deadline should wake the owner thread");
    assert_eq!(snapshot.deadline_count, 0);
    assert_eq!(snapshot.sinks[0].visits, 1);
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
    let updated = dataplane.update_output(first, 2).unwrap();
    assert_eq!(updated.generation, 2);
    assert_eq!(
        dataplane.update_output(first, 1),
        Err(restream_dataplane::CommandError::StaleGeneration)
    );
    assert_eq!(
        dataplane.remove_output_if_generation(first),
        Err(restream_dataplane::CommandError::StaleGeneration)
    );
    assert!(dataplane.wake_output(updated).unwrap());
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
    assert!(dataplane.remove_output(updated).unwrap());
    let recycled = dataplane.add_output(9).unwrap();
    assert_eq!(recycled.shard, updated.shard);
    assert_eq!(recycled.slot, updated.slot);
    assert!(recycled.generation > updated.generation);
    assert_eq!(
        dataplane.wake_output(updated),
        Err(restream_dataplane::CommandError::StaleGeneration)
    );
    dataplane.shutdown().unwrap();
}
