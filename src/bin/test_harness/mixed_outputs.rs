//! Mixed-runner output-matrix orchestration helpers.

use super::*;
use futures_util::stream::{FuturesUnordered, StreamExt};

/// Parameters for creating a homogeneous group of mixed-matrix outputs.
pub(crate) struct MixedGroupSpec<'a> {
    pub(crate) cfg: &'a str,
    pub(crate) group: &'a str,
    pub(crate) count: usize,
    pub(crate) encoding: &'a str,
    pub(crate) selected_audio_track: Option<usize>,
    pub(crate) expected_dimensions: Option<&'a str>,
    pub(crate) expected_audio_tracks: Option<usize>,
}

pub(crate) async fn add_mixed_group<F>(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    spec: MixedGroupSpec<'_>,
    url_for: F,
    output_ids: &mut Vec<String>,
) -> Result<(), String>
where
    F: Fn(usize) -> String,
{
    let started = Instant::now();
    let encoding = spec.encoding;
    let mut pending = FuturesUnordered::new();
    for index in 1..=spec.count {
        let name = format!("{}-{index}", spec.group);
        let url = url_for(index);
        pending.push(async move {
            let output_id = create_output(api, pipeline_id, &name, &url, encoding).await?;
            start_output(api, pipeline_id, &output_id).await?;
            Ok::<_, String>((index, name, url, output_id))
        });
    }
    while let Some(result) = pending.next().await {
        let (index, name, url, output_id) = result?;
        output_ids.push(output_id.clone());
        env.register_output_cell(HarnessOutputCell {
            scenario_id: spec.cfg.to_string(),
            batch_group: spec.group.to_string(),
            wave: 0,
            pipeline_id: pipeline_id.to_string(),
            output_id,
            output_name: name,
            cell_id: spec.group.to_string(),
            duplicate_index: index,
            protocol: infer_output_protocol(&url),
            encoding: spec.encoding.to_string(),
            selected_audio_track: spec.selected_audio_track,
            publish_url: url,
            read_url: None,
            expected_dimensions: spec.expected_dimensions.map(str::to_string),
            expected_audio_tracks: spec.expected_audio_tracks,
            terminal_stage: None,
        })?;
    }
    println!(
        "[mixed-input] added {} {} outputs for {}",
        spec.count, spec.group, spec.cfg
    );
    emit_mixed_timing(
        env,
        spec.cfg,
        &format!("outputs.create.{}", spec.group),
        "pass",
        started.elapsed(),
        Some(json!({
            "group": spec.group,
            "count": spec.count,
            "encoding": spec.encoding,
        })),
    )?;
    Ok(())
}

pub(crate) fn mixed_output_publish_url(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    index: usize,
) -> String {
    let output_name = mixed_output_instance_name(cfg, case.id(), index);
    match case.protocol() {
        MixedOutputProtocol::Rtmp => {
            format!("rtmp://127.0.0.1:{}/live/{output_name}", env.mtx_rtmp)
        }
        MixedOutputProtocol::Srt => {
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{output_name}",
                env.mtx_srt
            )
        }
    }
}

pub(crate) fn mixed_output_read_url(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    index: usize,
) -> String {
    let output_name = mixed_output_instance_name(cfg, case.id(), index);
    match case.protocol() {
        MixedOutputProtocol::Rtmp => mixed_output_publish_url(env, cfg, case, index),
        MixedOutputProtocol::Srt => {
            format!(
                "srt://127.0.0.1:{}?streamid=read:live/{output_name}&timeout=30000000",
                env.mtx_srt
            )
        }
    }
}

pub(crate) fn mixed_output_matrix_json(cases: &[MixedOutputCase]) -> Vec<Value> {
    cases
        .iter()
        .map(|case| {
            let mut value = json!({
                "id": case.id(),
                "protocol": mixed_output_protocol_name(case.protocol()),
                "encoding": case.encoding(),
                "expectedDimensions": case.expected_dimensions(),
                "expectedAudioTracks": case.expected_audio_tracks(),
            });
            if let Some(track) = case.selected_audio_track() {
                value["selectedAudioTrack"] = json!(track);
            }
            value
        })
        .collect()
}

pub(crate) async fn add_mixed_output_matrix_rows(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    restream_pid: u32,
    cfg: &str,
    cases: &[MixedOutputCase],
    output_ids: &mut Vec<String>,
) -> Result<(), String> {
    for case in cases {
        add_mixed_group(
            env,
            api,
            pipeline_id,
            MixedGroupSpec {
                cfg,
                group: case.id(),
                count: env.n_per_group,
                encoding: case.encoding(),
                selected_audio_track: case.selected_audio_track(),
                expected_dimensions: Some(case.expected_dimensions()),
                expected_audio_tracks: Some(case.expected_audio_tracks()),
            },
            |index| mixed_output_publish_url(env, cfg, case, index),
            output_ids,
        )
        .await?;
        if !env.skip_load {
            snapshot_mixed(
                env,
                restream_pid,
                cfg,
                &format!("after {} {} outputs", env.n_per_group, case.id()),
            )
            .await?;
        }
    }
    Ok(())
}
