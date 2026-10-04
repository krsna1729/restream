#!/usr/bin/env bash

run_common_concurrency_checks() {
  local run_step_fn="$1"

  for target in avio_loom egress_feed_wake_loom input_selection_loom ring_migration_loom ts_chunk_ring_loom ts_muxer_stage_loom transcoder_stage_loom; do
    "$run_step_fn" "loom-${target}" ./scripts/harness/loom-target.sh "$target"
  done

  "$run_step_fn" api-health \
    cargo test health_endpoint_exposes_probe_and_egress_fault_fields --test api -- --nocapture
  "$run_step_fn" api-output-recent-failure \
    cargo test output_status_and_health_preserve_recent_egress_failure_after_unregister --test api -- --nocapture
  "$run_step_fn" api-output-restart-retry \
    cargo test active_output_status_ignores_stale_retry_state_after_restart --test api -- --nocapture
  "$run_step_fn" output-status-active \
    cargo test active_output_status_matches_health_runtime_fields --test output_status_contract -- --nocapture
  "$run_step_fn" output-status-stalled \
    cargo test stalled_output_status_matches_health_runtime_fields --test output_status_contract -- --nocapture
  "$run_step_fn" api-disconnect-clears \
    cargo test health_endpoint_clears_recent_disconnect_details_after_reconnect --test api -- --nocapture
  "$run_step_fn" api-disconnect-flapping \
    cargo test health_endpoint_surfaces_repeated_transient_disconnects_as_flapping --test api -- --nocapture
  "$run_step_fn" api-egress-flapping \
    cargo test recovered_output_surfaces_flapping_after_repeated_sink_failures --test api -- --nocapture
  "$run_step_fn" db-stale-job-update \
    cargo test stale_job_update_cannot_clobber_replacement_attempt --test db -- --nocapture
  "$run_step_fn" db-multiple-stale-job-updates \
    cargo test multiple_stale_job_updates_cannot_clobber_newest_attempt --test db -- --nocapture
  "$run_step_fn" lib-stale-ingest-unregister \
    cargo test stale_ingest_unregister_cannot_clobber_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-ingest-disconnect \
    cargo test stale_ingest_disconnect_cannot_poison_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-unregister \
    cargo test stale_egress_unregister_cannot_clobber_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-error \
    cargo test stale_egress_error_cannot_poison_replacement_attempt --lib -- --nocapture
  "$run_step_fn" lib-stale-egress-queue \
    cargo test stale_egress_queue_removal_cannot_drop_replacement_queue --lib -- --nocapture
  "$run_step_fn" ring-proptest \
    cargo test prop_no_loss_no_gap_no_duplication --test ring_migration -- --nocapture
  "$run_step_fn" ring-multi-reader-proptest \
    cargo test prop_multi_reader_migration_preserves_each_reader_order --test ring_migration -- --nocapture
  "$run_step_fn" input-selection-proptest \
    cargo test gate_matches_sequential_selection_model --test input_selection -- --nocapture
  "$run_step_fn" standby-gop-proptest \
    cargo test cache_never_exceeds_its_declared_limits --test standby_gop -- --nocapture
  "$run_step_fn" lib-avio-batch \
    cargo test write_batch_round_trips_random_chunks --lib -- --nocapture
  "$run_step_fn" lib-avio-unit \
    cargo test 'media::avio::tests' --lib -- --nocapture
  "$run_step_fn" lib-srt-stream-id-normalization \
    cargo test media::srt_stream_id::tests --lib -- --nocapture
  "$run_step_fn" lib-srt-ingress-owner \
    cargo test media::srt::ingress_live_tests --lib -- --nocapture
  "$run_step_fn" lib-srt-ingress-bridges \
    cargo test media::srt::ingress_bridge_tests --lib -- --nocapture
  "$run_step_fn" lib-srt-ingress-admission \
    cargo test media::srt::ingress_admission --lib -- --nocapture
  "$run_step_fn" external-transcoder-routing \
    cargo test external_output_stream_idx_routes_known_tracks_without_aliasing --lib -- --nocapture
  "$run_step_fn" external-transcoder-routing-proptest \
    cargo test proptest_external_output_dts_routing_preserves_per_stream_monotonicity --lib -- --nocapture
  "$run_step_fn" external-transcoder-h264-live \
    cargo test external_720p_stage_emits_live_packets_for_h264_marker_fixture --lib -- --nocapture
  "$run_step_fn" external-transcoder-h264-dts-remux \
    cargo test external_1080p_stage_remuxes_marker_fixture_with_monotone_dts --lib -- --nocapture
  "$run_step_fn" internal-transcoder-chunked-scale \
    cargo test internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order --test transcoder -- --nocapture
  "$run_step_fn" internal-transcoder-source-proptest \
    cargo test prop_source_stage_chunked_input_preserves_per_stream_dts_order --test transcoder -- --nocapture
  "$run_step_fn" internal-transcoder-replacement-metadata \
    cargo test replacement_video_stage_preserves_codec_hint_and_audio_tracks --test transcoder -- --nocapture
  "$run_step_fn" hls-segment-dts-boundaries \
    cargo test hls_segment_boundaries_preserve_non_decreasing_dts_per_stream --lib -- --nocapture
  "$run_step_fn" recording-remux-continuity-retention-disabled \
    cargo test remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_disabled --lib -- --nocapture
  "$run_step_fn" recording-remux-continuity-retention-enabled \
    cargo test remux_recording_to_mp4_preserves_timestamp_continuity_when_retention_enabled --lib -- --nocapture
  "$run_step_fn" test-harness-process-lifecycle \
    cargo test --bin test_harness tests::kill_and_wait_child_terminates_spawned_process -- --exact --nocapture
  "$run_step_fn" rtmp-ingress-listener-shutdown \
    cargo test compio_rtmp_listener_shutdown_joins_acceptor_and_session_workers --lib -- --nocapture
  "$run_step_fn" rtmp-sharded-ingress-lifecycle \
    cargo test media::rtmp::listener::tests --lib
  "$run_step_fn" lib-compio-rtmp-readiness-fairness \
    cargo test media::egress::backends::compio_tcp::tests --lib -- --nocapture
  "$run_step_fn" test-harness-slow-sink-sibling-count \
    cargo test --bin test_harness tests::fault_output_stall_sibling_count_honors_n_per_group_cap -- --exact --nocapture
  "$run_step_fn" lib-recent-egress \
    cargo test recent_egress --lib -- --nocapture
  "$run_step_fn" lib-ingest-grace \
    cargo test recent_ingest_disconnect_respects_grace_window --lib -- --nocapture
  "$run_step_fn" lib-ingest-flap-window \
    cargo test build_recent_ingest_outcome_resets_flap_streak_outside_window --lib -- --nocapture
  "$run_step_fn" lib-ingest-proptest \
    cargo test prop_ingest_lifecycle_preserves_health_invariants --lib -- --nocapture
  "$run_step_fn" lib-egress-flap-window \
    cargo test build_recent_egress_outcome_resets_flap_streak_outside_window --lib -- --nocapture
  "$run_step_fn" lib-health-reconnect-flapping \
    cargo test health_snapshot_surfaces_flapping_after_repeated_reconnects --lib -- --nocapture
  "$run_step_fn" lib-health-egress-flapping \
    cargo test health_snapshot_surfaces_flapping_after_repeated_egress_recoveries --lib -- --nocapture
  "$run_step_fn" lib-late-retry-state \
    cargo test late_retry_state_update_is_ignored_after_output_restarts --lib -- --nocapture
  "$run_step_fn" lib-multi-late-retry-state \
    cargo test repeated_late_retry_updates_cannot_poison_newest_output_attempt --lib -- --nocapture
  "$run_step_fn" lib-output-retry-backoff \
    cargo test output_status_surfaces_retry_backoff_after_failure --lib -- --nocapture
  "$run_step_fn" lib-egress-proptest \
    cargo test prop_egress_lifecycle_preserves_runtime_and_health_invariants --lib -- --nocapture
  "$run_step_fn" lib-egress-leaf-cursor-priming \
    cargo test first_visit_primes --lib -- --nocapture
  "$run_step_fn" lib-egress-leaf-live-start \
    cargo test fresh_leaf_first_visit --lib -- --nocapture
  "$run_step_fn" recording-drain-bounded-on-cancel \
    cargo test media::recording::tests::drain_ready_bursts --lib -- --nocapture
  "$run_step_fn" lib-media-executor \
    cargo test media::executor::tests --lib
  "$run_step_fn" lib-egress-sizing \
    cargo test media::egress::sizing --lib
  "$run_step_fn" lib-egress-resize \
    cargo test media::egress::runtime --lib
  "$run_step_fn" lib-media-control-isolation \
    cargo test while_control_thread_is_blocked --lib
  "$run_step_fn" lib-media-file-ingest \
    cargo test media::external_file_ingest::tests --lib
  "$run_step_fn" recording-media-owner-abort \
    cargo test aborting_control_owner_closes_media_feeder_and_writer --lib
  "$run_step_fn" hls-media-owner-abort \
    cargo test control_owner_abort_flushes_final_segment --lib
  "$run_step_fn" recording-media-writer-failure \
    cargo test recording_media_writer_failure_reports_failed_without_finalization --lib
  "$run_step_fn" hls-media-replacement \
    cargo test detached_teardown_preserves_replacement --lib
}
