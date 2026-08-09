#!/usr/bin/env bash

run_common_concurrency_checks() {
  local run_step_fn="$1"

  for target in avio_loom egress_feed_wake_loom input_selection_loom ring_migration_loom ts_chunk_ring_loom ts_muxer_stage_loom transcoder_stage_loom; do
    "$run_step_fn" "loom-${target}" ./scripts/harness/loom-target.sh "$target"
  done

  "$run_step_fn" api-health \
    scripts/build/resource-limit.sh cargo test health_endpoint_exposes_probe_and_egress_fault_fields --test api -- --nocapture
  "$run_step_fn" api-output-recent-failure \
    scripts/build/resource-limit.sh cargo test output_status_and_health_preserve_recent_egress_failure_after_unregister --test api -- --nocapture
  "$run_step_fn" api-output-restart-retry \
    scripts/build/resource-limit.sh cargo test active_output_status_ignores_stale_retry_state_after_restart --test api -- --nocapture
  "$run_step_fn" output-status-active \
    scripts/build/resource-limit.sh cargo test active_output_status_matches_health_runtime_fields --test output_status_contract -- --nocapture
  "$run_step_fn" output-status-stalled \
    scripts/build/resource-limit.sh cargo test stalled_output_status_matches_health_runtime_fields --test output_status_contract -- --nocapture
  "$run_step_fn" api-disconnect-clears \
    scripts/build/resource-limit.sh cargo test health_endpoint_clears_recent_disconnect_details_after_reconnect --test api -- --nocapture
  "$run_step_fn" api-disconnect-flapping \
    scripts/build/resource-limit.sh cargo test health_endpoint_surfaces_repeated_transient_disconnects_as_flapping --test api -- --nocapture
  "$run_step_fn" api-egress-flapping \
    scripts/build/resource-limit.sh cargo test recovered_output_surfaces_flapping_after_repeated_sink_failures --test api -- --nocapture
  "$run_step_fn" db-stale-job-update \
    scripts/build/resource-limit.sh cargo test stale_job_update_cannot_clobber_replacement_attempt --test db -- --nocapture
  "$run_step_fn" db-multiple-stale-job-updates \
    scripts/build/resource-limit.sh cargo test multiple_stale_job_updates_cannot_clobber_newest_attempt --test db -- --nocapture
  "$run_step_fn" lib-stale-ingest-unregister \
    scripts/build/resource-limit.sh cargo test stale_ingest_unregister_cannot_clobber_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-ingest-disconnect \
    scripts/build/resource-limit.sh cargo test stale_ingest_disconnect_cannot_poison_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-unregister \
    scripts/build/resource-limit.sh cargo test stale_egress_unregister_cannot_clobber_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-error \
    scripts/build/resource-limit.sh cargo test stale_egress_error_cannot_poison_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-queue \
    scripts/build/resource-limit.sh cargo test stale_egress_queue_removal_cannot_drop_replacement_queue --lib -- --nocapture
  "$run_step_fn" ring-proptest \
    scripts/build/resource-limit.sh cargo test prop_no_loss_no_gap_no_duplication --test ring_migration -- --nocapture
  "$run_step_fn" ring-multi-reader-proptest \
    scripts/build/resource-limit.sh cargo test prop_multi_reader_migration_preserves_each_reader_order --test ring_migration -- --nocapture
  "$run_step_fn" input-selection-proptest \
    scripts/build/resource-limit.sh cargo test gate_matches_sequential_selection_model --test input_selection -- --nocapture
  "$run_step_fn" standby-gop-proptest \
    scripts/build/resource-limit.sh cargo test cache_never_exceeds_its_declared_limits --test standby_gop -- --nocapture
  "$run_step_fn" lib-avio-batch \
    scripts/build/resource-limit.sh cargo test write_batch_round_trips_random_chunks --lib -- --nocapture
  "$run_step_fn" lib-avio-unit \
    scripts/build/resource-limit.sh cargo test 'media::avio::tests' --lib -- --nocapture
  "$run_step_fn" lib-srt-epoll \
    scripts/build/resource-limit.sh cargo test epoll_waiter_coordination --lib -- --nocapture
  "$run_step_fn" lib-srt-readiness-loom \
    scripts/build/resource-limit.sh cargo test loom_srt_readiness_retry_does_not_depend_on_epoll_wake --lib -- --nocapture
  "$run_step_fn" lib-srt-readiness-proptest \
    scripts/build/resource-limit.sh cargo test proptest_srt_readiness_retry_model_never_requires_epoll_wake --lib -- --nocapture
  "$run_step_fn" lib-srt-stream-id-normalization \
    scripts/build/resource-limit.sh cargo test srt_stream_ids_normalize_equivalent --lib -- --nocapture
  "$run_step_fn" lib-srt-sender-semaphore \
    scripts/build/resource-limit.sh cargo test srt_sender_semaphore --lib -- --nocapture
  "$run_step_fn" external-transcoder-routing \
    scripts/build/resource-limit.sh cargo test external_output_stream_idx_routes_known_tracks_without_aliasing --lib -- --nocapture
  "$run_step_fn" external-transcoder-routing-proptest \
    scripts/build/resource-limit.sh cargo test proptest_external_output_dts_routing_preserves_per_stream_monotonicity --lib -- --nocapture
  "$run_step_fn" external-transcoder-h264-live \
    scripts/build/resource-limit.sh cargo test external_720p_stage_emits_live_packets_for_h264_marker_fixture --lib -- --nocapture
  "$run_step_fn" external-transcoder-h264-dts-remux \
    scripts/build/resource-limit.sh cargo test external_1080p_stage_remuxes_marker_fixture_with_monotone_dts --lib -- --nocapture
  "$run_step_fn" internal-transcoder-chunked-scale \
    scripts/build/resource-limit.sh cargo test internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order --test transcoder -- --nocapture
  "$run_step_fn" internal-transcoder-source-proptest \
    scripts/build/resource-limit.sh cargo test prop_source_stage_chunked_input_preserves_per_stream_dts_order --test transcoder -- --nocapture
  "$run_step_fn" internal-transcoder-replacement-metadata \
    scripts/build/resource-limit.sh cargo test replacement_video_stage_preserves_codec_hint_and_audio_tracks --test transcoder -- --nocapture
  "$run_step_fn" hls-segment-dts-boundaries \
    scripts/build/resource-limit.sh cargo test hls_segment_boundaries_preserve_non_decreasing_dts_per_stream --lib -- --nocapture
  "$run_step_fn" recording-remux-continuity-retention-disabled \
    scripts/build/resource-limit.sh cargo test remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled --lib -- --nocapture
  "$run_step_fn" recording-remux-continuity-retention-enabled \
    scripts/build/resource-limit.sh cargo test remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled --lib -- --nocapture
  "$run_step_fn" test-harness-process-lifecycle \
    scripts/build/resource-limit.sh cargo test --bin test_harness tests::kill_and_wait_child_terminates_spawned_process -- --exact --nocapture
  "$run_step_fn" test-harness-slow-sink-sibling-count \
    scripts/build/resource-limit.sh cargo test --bin test_harness tests::fault_output_stall_sibling_count_honors_n_per_group_cap -- --exact --nocapture
  "$run_step_fn" lib-recent-egress \
    scripts/build/resource-limit.sh cargo test recent_egress --lib -- --nocapture
  "$run_step_fn" lib-ingest-grace \
    scripts/build/resource-limit.sh cargo test recent_ingest_disconnect_respects_grace_window --lib -- --nocapture
  "$run_step_fn" lib-ingest-flap-window \
    scripts/build/resource-limit.sh cargo test build_recent_ingest_outcome_resets_flap_streak_outside_window --lib -- --nocapture
  "$run_step_fn" lib-ingest-proptest \
    scripts/build/resource-limit.sh cargo test prop_ingest_lifecycle_preserves_health_invariants --lib -- --nocapture
  "$run_step_fn" lib-egress-flap-window \
    scripts/build/resource-limit.sh cargo test build_recent_egress_outcome_resets_flap_streak_outside_window --lib -- --nocapture
  "$run_step_fn" lib-health-reconnect-flapping \
    scripts/build/resource-limit.sh cargo test health_snapshot_surfaces_flapping_after_repeated_reconnects --lib -- --nocapture
  "$run_step_fn" lib-health-egress-flapping \
    scripts/build/resource-limit.sh cargo test health_snapshot_surfaces_flapping_after_repeated_egress_recoveries --lib -- --nocapture
  "$run_step_fn" lib-late-retry-state \
    scripts/build/resource-limit.sh cargo test late_retry_state_update_is_ignored_after_output_restarts --lib -- --nocapture
  "$run_step_fn" lib-multi-late-retry-state \
    scripts/build/resource-limit.sh cargo test repeated_late_retry_updates_cannot_poison_newest_output_attempt --lib -- --nocapture
  "$run_step_fn" lib-output-retry-backoff \
    scripts/build/resource-limit.sh cargo test output_status_surfaces_retry_backoff_after_failure --lib -- --nocapture
  "$run_step_fn" lib-egress-proptest \
    scripts/build/resource-limit.sh cargo test prop_egress_lifecycle_preserves_runtime_and_health_invariants --lib -- --nocapture
  "$run_step_fn" recording-drain-bounded-on-cancel \
    scripts/build/resource-limit.sh cargo test media::recording::tests::drain_ready_bursts --lib -- --nocapture
}
