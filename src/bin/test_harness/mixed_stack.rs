//! Mixed-runner shared stack lifecycle helpers.

use super::*;

/// Shared live stack for mixed harness waves.
pub(crate) struct MixedHarnessStack {
    pub(crate) env: MixedEnv,
    pub(crate) mediamtx: Child,
    pub(crate) restream: Child,
    pub(crate) api: RampApi,
    pub(crate) restream_pid: u32,
}

pub(crate) async fn start_mixed_harness_stack(env: MixedEnv) -> Result<MixedHarnessStack, String> {
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&env.media_dir).map_err(|e| e.to_string())?;
    let mediamtx = start_mixed_mediamtx(&env).await?;
    let restream = start_mixed_restream(&env).await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    Ok(MixedHarnessStack {
        env,
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

pub(crate) async fn stop_mixed_harness_stack(stack: &mut MixedHarnessStack) {
    stop_child(&mut stack.restream).await;
    stop_child(&mut stack.mediamtx).await;
}

pub(crate) fn bind_mixed_env_to_shared_stack(env: &mut MixedEnv, stack_env: &MixedEnv) {
    env.restream_http = stack_env.restream_http;
    env.restream_rtmp = stack_env.restream_rtmp;
    env.restream_srt = stack_env.restream_srt;
    env.mtx_rtmp = stack_env.mtx_rtmp;
    env.mtx_srt = stack_env.mtx_srt;
    env.mtx_hls = stack_env.mtx_hls;
    env.mtx_api = stack_env.mtx_api;
    env.media_dir = stack_env.media_dir.clone();
    env.restream_log = stack_env.restream_log.clone();
    env.mediamtx_log = stack_env.mediamtx_log.clone();
    env.mediamtx_config = stack_env.mediamtx_config.clone();
    env.restream_db_path = stack_env.restream_db_path.clone();
}
