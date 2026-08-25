#[path = "core/history.rs"]
mod history;
#[path = "core/ports.rs"]
mod ports;
#[path = "core/process.rs"]
mod process;
#[path = "core/profile.rs"]
mod profile;
#[path = "core/setup.rs"]
mod setup;
#[path = "core/srt_crypto.rs"]
mod srt_crypto;

#[allow(unused_imports)]
pub(crate) use history::{
    get_logs, log_has_correlation_id, parse_log_fields, verify_api_smoke_history_contract,
    verify_external_transcoder_history_contract, verify_live_history_contract,
};
#[allow(unused_imports)]
pub(crate) use ports::{
    HarnessPortDefaults, TestPorts, env_or_allocated_port, env_or_allocated_port_range,
    harness_port_defaults,
};
#[allow(unused_imports)]
pub(crate) use process::{
    FfmpegStats, append_line, ffmpeg_pipe1_stats, login_api, proc_net_has_listening_port,
    process_cpu_pct, process_rss_kb, start_restream_api, start_restream_child,
    start_restream_child_in_media_dir, start_restream_child_opts, start_restream_child_with_env,
    wait_for_http_ok, wait_for_tcp_listener_ready,
};
#[allow(unused_imports)]
pub(crate) use profile::{
    default_restream_bin, default_work_db_path, ensure_measurement_profile,
    ensure_msr_nofile_limit, harness_runtime_max_blocking_threads, harness_runtime_worker_threads,
    is_optimized_profile, maybe_reexec_in_port_namespace, measurement_profile_ok,
    measurement_profile_ok_with_explicit, netns_available, restream_bin_is_explicit,
    strip_netns_opt,
};
#[allow(unused_imports)]
pub(crate) use setup::{
    MEDIAMTX_CONFIG_ENV_NAMES, absolute_path, artifact_path, command_with_optional_cgroup,
    env_flag, env_secs, env_usize, maybe_global_process_cleanup, maybe_prune_old_artifacts,
    mixed_command_artifact_path, remove_mediamtx_config_env,
};
pub(crate) use srt_crypto::{
    HarnessSrtCrypto, append_srt_crypto, apply_harness_srt_listener_env, apply_srt_listener_env,
    harness_srt_crypto_from_env, parse_srt_crypto_variants,
};
