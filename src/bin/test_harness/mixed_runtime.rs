//! Runtime process and publisher bootstrap helpers for mixed scenarios.

use super::*;

pub(crate) async fn start_mixed_restream(env: &MixedEnv) -> Result<Child, String> {
    std::fs::create_dir_all(&env.media_dir).map_err(|e| e.to_string())?;
    start_restream_child_in_media_dir(
        &env.restream_bin,
        &TestPorts {
            http: env.restream_http,
            rtmp: env.restream_rtmp,
            srt: env.restream_srt,
        },
        &env.restream_db_path,
        &env.restream_log,
        &env.media_dir,
    )
    .await
}

pub(crate) async fn start_mixed_mediamtx(env: &MixedEnv) -> Result<Child, String> {
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: yes\nhlsAddress: :{}\nhlsPartDuration: 200ms\nhlsSegmentDuration: 2s\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_hls, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = command_with_optional_cgroup("mediamtx", &format!("mediamtx-{}", env.mtx_api))
        .arg(&env.mediamtx_config)
        .env_remove("MTX_RTMP")
        .env_remove("MTX_SRT")
        .env_remove("MTX_HLS")
        .env_remove("MTX_API")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

pub(crate) async fn spawn_mixed_live_publisher(
    env: &MixedEnv,
    case: MixedInputCase,
    stream_key: &str,
) -> Result<Child, String> {
    let log_path = env
        .work_dir
        .join(format!("{}.publisher.log", case.scenario_id()));
    let fixture = mixed_input_fixture(case)?;
    let (url, format) = match case.protocol() {
        MixedInputProtocol::Rtmp => (
            format!("rtmp://127.0.0.1:{}/live/{stream_key}", env.restream_rtmp),
            "flv",
        ),
        MixedInputProtocol::Srt => (
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&latency=200000",
                env.restream_srt
            ),
            "mpegts",
        ),
        MixedInputProtocol::File => {
            return Err(format!(
                "{} uses file ingest and cannot spawn a live publisher",
                case.scenario_id()
            ));
        }
    };
    spawn_publisher_with_selection(
        &fixture,
        &url,
        format,
        PublishTrackSelection::PrimaryAv,
        Some(&log_path),
    )
}

pub(crate) async fn spawn_mixed_srt_multi_publisher(
    env: &MixedEnv,
    case: MixedInputCase,
    stream_key: &str,
) -> Result<Child, String> {
    let log_path = env
        .work_dir
        .join(format!("{}.publisher.log", case.scenario_id()));
    let fixture = mixed_input_fixture(case)?;
    spawn_publisher_with_selection(
        &fixture,
        &format!(
            "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&latency=200000",
            env.restream_srt
        ),
        "mpegts",
        PublishTrackSelection::AllStreams,
        Some(&log_path),
    )
}
