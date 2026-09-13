use restream_dataplane::{Dataplane, ShardConfig};

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
    let snapshot = dataplane.snapshot().unwrap();
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
