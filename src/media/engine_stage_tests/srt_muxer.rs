use super::*;

#[tokio::test]
async fn srt_muxer_assignment_creates_new_shards_at_output_threshold() {
    let engine = engine_with_srt_muxer_caps(2, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:0");
    assert_eq!(third, "source:srt-mux-shard:1");
}

#[tokio::test]
async fn srt_muxer_assignment_reuses_freed_empty_shard() {
    let engine = engine_with_srt_muxer_caps(1, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:1");
    assert_eq!(third, "source:srt-mux-shard:0");
}

#[tokio::test]
async fn srt_muxer_assignment_degrades_to_least_loaded_at_max_shards() {
    let engine = engine_with_srt_muxer_caps(1, 2);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;
    let fourth = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-4", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:1");
    assert_eq!(third, "source:srt-mux-shard:0");
    assert_eq!(fourth, "source:srt-mux-shard:1");
}

#[tokio::test]
async fn stale_srt_muxer_release_cannot_remove_replacement_assignment() {
    let engine = engine_with_srt_muxer_caps(1, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 1)
        .await;
    let replacement = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 2)
        .await;
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-race", 1)
        .await;
    let still_current = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 2)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(replacement, "source:srt-mux-shard:0");
    assert_eq!(still_current, replacement);
}

#[tokio::test]
async fn empty_srt_muxer_shard_cancels_and_removes_ts_stage() {
    let engine = Arc::new(engine_with_srt_muxer_caps(1, 8));
    let source_ring = Arc::new(RingBuffer::new(8));
    let stage_key = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let ts_ring = engine
        .get_or_create_ts_muxer_stage("pipe-1", &stage_key, source_ring)
        .await;

    assert!(
        engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe-1:source:srt-mux-shard:0")
    );
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;

    assert!(ts_ring.cancel.is_cancelled());
    assert!(
        !engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe-1:source:srt-mux-shard:0")
    );
}

#[derive(Clone, Copy, Debug)]
enum SrtMuxerLifecycleOp {
    Assign { output: usize, attempt_delta: u8 },
    RepeatAssign { output: usize },
    ReleaseCurrent { output: usize },
    ReleaseStale { output: usize },
}

fn srt_muxer_lifecycle_op_strategy() -> impl Strategy<Value = SrtMuxerLifecycleOp> {
    prop_oneof![
        (0usize..12, 0u8..3).prop_map(|(output, attempt_delta)| {
            SrtMuxerLifecycleOp::Assign {
                output,
                attempt_delta,
            }
        }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::RepeatAssign { output }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::ReleaseCurrent { output }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::ReleaseStale { output }),
    ]
}

fn parse_srt_muxer_shard_index(stage_key: &str) -> usize {
    stage_key
        .rsplit_once(":srt-mux-shard:")
        .and_then(|(_, shard)| shard.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("stage key should contain shard index: {stage_key}"))
}

async fn assert_srt_muxer_pool_matches_model(
    engine: &MediaEngine,
    model: &HashMap<String, (u64, usize)>,
    max_shards: usize,
) {
    let pools = engine.stages.srt_muxer_shards.read().await;
    let pool = pools.get("pipe-1\u{1f}source");

    if model.is_empty() {
        assert!(
            pool.is_none_or(|pool| pool.is_empty()),
            "empty model should leave no live shard assignments: {pool:?}"
        );
        return;
    }

    let pool = pool.expect("non-empty model should have a shard pool");
    let (assignments, shard_occupancy, retiring_shards) = pool.test_snapshot();
    assert_eq!(assignments.len(), model.len());
    assert!(
        shard_occupancy.len() <= max_shards,
        "shard count must stay capped"
    );

    let mut expected_occupancy = vec![0usize; shard_occupancy.len()];
    let mut expected_assignments = HashMap::new();
    for (output, (attempt, shard)) in model {
        assert!(
            *shard < max_shards,
            "model shard index must stay below configured cap"
        );
        if *shard >= expected_occupancy.len() {
            expected_occupancy.resize(*shard + 1, 0);
        }
        expected_occupancy[*shard] += 1;
        expected_assignments.insert(
            output.clone(),
            SrtMuxerAssignment {
                attempt_id: *attempt,
                shard_index: *shard,
            },
        );
    }

    assert_eq!(assignments, expected_assignments);
    assert_eq!(shard_occupancy, expected_occupancy);
    for retiring in &retiring_shards {
        assert_eq!(
            shard_occupancy.get(*retiring).copied().unwrap_or_default(),
            0,
            "only empty shards may be marked retiring"
        );
    }

    let assigned_shards = model
        .values()
        .map(|(_, shard)| *shard)
        .collect::<HashSet<_>>();
    assert!(
        assigned_shards.len() <= max_shards,
        "live assignment fanout must stay within max shards"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn proptest_srt_muxer_shard_lifecycle_matches_model(
        max_outputs_per_shard in 1usize..=4,
        max_shards in 1usize..=5,
        ops in prop::collection::vec(srt_muxer_lifecycle_op_strategy(), 1..96),
    ) {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            let engine = engine_with_srt_muxer_caps(max_outputs_per_shard, max_shards);
            let mut model: HashMap<String, (u64, usize)> = HashMap::new();
            let mut next_attempt_by_output = [1_u64; 12];
            let mut stale_attempt_by_output: [Option<u64>; 12] = [None; 12];

            for op in ops {
                match op {
                    SrtMuxerLifecycleOp::Assign { output, attempt_delta } => {
                        let output_id = format!("out-{output}");
                        if let Some((attempt, _)) = model.get(&output_id).copied() {
                            stale_attempt_by_output[output] = Some(attempt);
                        }
                        next_attempt_by_output[output] =
                            next_attempt_by_output[output].saturating_add(u64::from(attempt_delta) + 1);
                        let attempt = next_attempt_by_output[output];
                        let stage_key = engine
                            .assign_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.insert(output_id, (attempt, parse_srt_muxer_shard_index(&stage_key)));
                    }
                    SrtMuxerLifecycleOp::RepeatAssign { output } => {
                        let output_id = format!("out-{output}");
                        let attempt = model
                            .get(&output_id)
                            .map(|(attempt, _)| *attempt)
                            .unwrap_or(next_attempt_by_output[output]);
                        let stage_key = engine
                            .assign_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.insert(output_id, (attempt, parse_srt_muxer_shard_index(&stage_key)));
                    }
                    SrtMuxerLifecycleOp::ReleaseCurrent { output } => {
                        let output_id = format!("out-{output}");
                        let attempt = model
                            .get(&output_id)
                            .map(|(attempt, _)| *attempt)
                            .unwrap_or(next_attempt_by_output[output]);
                        engine
                            .release_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.remove(&output_id);
                    }
                    SrtMuxerLifecycleOp::ReleaseStale { output } => {
                        let output_id = format!("out-{output}");
                        let stale_attempt = stale_attempt_by_output[output]
                            .or_else(|| model.get(&output_id).map(|(attempt, _)| attempt.saturating_sub(1)))
                            .unwrap_or(0);
                        engine
                            .release_srt_egress_muxer_stage("pipe-1", "source", &output_id, stale_attempt)
                            .await;
                    }
                }

                assert_srt_muxer_pool_matches_model(&engine, &model, max_shards).await;
            }
        });
    }
}
