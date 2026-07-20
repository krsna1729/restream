use super::*;

fn msr_ffprobe_sample_count(output_count: usize) -> usize {
    let default = if std::env::var("MSR_FULL").ok().as_deref() == Some("1") {
        "60"
    } else {
        "4"
    };
    env_usize("MSR_FFPROBE_SAMPLE_COUNT", default.parse().unwrap())
        .min(output_count)
        .max(usize::from(output_count > 0))
}

fn msr_ffprobe_seed() -> u64 {
    std::env::var("MSR_FFPROBE_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0x5eed_5eed_cafe_babe)
}

pub(super) fn msr_ffprobe_detection_confidence(
    sample_count: usize,
    population: usize,
    defect_rate: f64,
) -> f64 {
    if population == 0 || sample_count == 0 || defect_rate <= 0.0 {
        return 0.0;
    }
    let bad = ((population as f64) * defect_rate)
        .ceil()
        .clamp(1.0, population as f64) as usize;
    let sample_count = sample_count.min(population);
    let mut miss_probability = 1.0f64;
    for draw in 0..sample_count {
        let remaining = population.saturating_sub(draw);
        let good_remaining = population.saturating_sub(bad).saturating_sub(draw);
        if remaining == 0 {
            break;
        }
        miss_probability *= good_remaining as f64 / remaining as f64;
    }
    (1.0 - miss_probability).clamp(0.0, 1.0)
}

pub(super) fn msr_ffprobe_sample_outputs(
    outputs: &[MsrOutputSpec],
    sample_count: usize,
    seed: u64,
) -> Vec<MsrOutputSpec> {
    if outputs.is_empty() || sample_count == 0 {
        return Vec::new();
    }
    let mut indexes = Vec::with_capacity(outputs.len());
    let mut state = seed;
    for index in 0..outputs.len() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        indexes.push((state, index));
    }
    indexes.sort_unstable_by_key(|(rank, _)| *rank);
    let mut selected = indexes
        .into_iter()
        .take(sample_count.min(outputs.len()))
        .map(|(_, index)| index)
        .collect::<Vec<_>>();
    if !selected
        .iter()
        .any(|index| outputs[*index].protocol == MsrProtocol::Srt)
        && let Some(srt_index) = outputs
            .iter()
            .position(|output| output.protocol == MsrProtocol::Srt)
    {
        if selected.len() < sample_count.min(outputs.len()) {
            selected.push(srt_index);
        } else if let Some(last) = selected.last_mut() {
            *last = srt_index;
        }
    }
    selected.sort_unstable();
    selected.dedup();
    selected
        .into_iter()
        .map(|index| outputs[index].clone())
        .collect()
}

fn validate_msr_ffprobe_sample(output: &MsrOutputSpec, probe: &Value) -> Result<Value, String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or_else(|| format!("{} ffprobe output has no streams array", output.name))?;
    let video_stream = streams
        .iter()
        .find(|stream| stream["codec_type"].as_str() == Some("video"))
        .ok_or_else(|| format!("{} ffprobe did not find a video stream", output.name))?;
    let video_codec = video_stream["codec_name"].as_str().unwrap_or("");
    if !matches!(video_codec, "h264" | "hevc") {
        return Err(format!(
            "{} ffprobe found unexpected video codec {video_codec:?}",
            output.name
        ));
    }
    let audio_streams = streams
        .iter()
        .filter(|stream| stream["codec_type"].as_str() == Some("audio"))
        .count();
    if audio_streams == 0 {
        return Err(format!(
            "{} ffprobe did not find an audio stream",
            output.name
        ));
    }
    let video_index = video_stream["index"].as_i64().unwrap_or(0);
    let mut video_packets = 0usize;
    let mut last_dts = None;
    for packet in probe["packets"].as_array().into_iter().flatten() {
        if packet["stream_index"].as_i64() != Some(video_index) {
            continue;
        }
        let Some(dts) = packet["dts_time"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        if let Some(previous) = last_dts
            && dts < previous
        {
            return Err(format!(
                "{} ffprobe observed non-monotone video DTS ({previous} -> {dts})",
                output.name
            ));
        }
        last_dts = Some(dts);
        video_packets += 1;
    }
    if video_packets == 0 {
        return Err(format!(
            "{} ffprobe did not capture any video packets",
            output.name
        ));
    }
    Ok(json!({
        "videoCodec": video_codec,
        "audioStreams": audio_streams,
        "videoPackets": video_packets,
    }))
}

pub(super) async fn run_msr_ffprobe_checkpoint(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    outputs: &[MsrOutputSpec],
) -> Result<Vec<Value>, String> {
    let sample_count = msr_ffprobe_sample_count(outputs.len());
    let seed = msr_ffprobe_seed();
    let duration = env_secs("MSR_FFPROBE_SAMPLE_SECS", 5);
    let probesize = std::env::var("MSR_FFPROBE_PROBESIZE").unwrap_or_else(|_| "10M".to_string());
    let analyzeduration =
        std::env::var("MSR_FFPROBE_ANALYZEDURATION").unwrap_or_else(|_| "10M".to_string());
    let defect_rate = std::env::var("MSR_FFPROBE_DEFECT_RATE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.05);
    let samples = msr_ffprobe_sample_outputs(outputs, sample_count, seed);
    let confidence = msr_ffprobe_detection_confidence(samples.len(), outputs.len(), defect_rate);
    let mut checks = Vec::new();
    for output in samples {
        let url = msr_read_url(env, &output);
        let label = format!("{}-{}-{}", MSR_MODE, checkpoint, output.name);
        let probe_path = env
            .work_dir
            .join(format!("{}.ffprobe.json", safe_artifact_stem(&label)));
        let probe =
            ffprobe_live_sample(&url, &probe_path, duration, &probesize, &analyzeduration).await?;
        let shape = validate_msr_ffprobe_sample(&output, &probe)?;
        let check = json!({
            "kind": "msrFfprobeSample",
            "mode": MSR_MODE,
            "checkpoint": checkpoint,
            "sampleCount": sample_count,
            "population": outputs.len(),
            "seed": seed,
            "durationSecs": duration,
            "probesize": probesize,
            "analyzeduration": analyzeduration,
            "defectRateAssumption": defect_rate,
            "detectionConfidence": confidence,
            "output": {
                "name": output.name,
                "ordinal": output.ordinal,
                "rank": output.rank,
                "protocol": output.protocol.label(),
                "rtmpMode": output.rtmp_mode_name(),
                "encoding": output.encoding,
            },
            "url": url,
            "artifact": probe_path,
            "shape": shape,
        });
        append_line(
            &env.samples_jsonl,
            &format!("{}\n", serde_json::to_string(&check).unwrap()),
        )?;
        checks.push(check);
    }
    Ok(checks)
}

pub(super) fn msr_signal_calibration_enabled() -> bool {
    match std::env::var("MSR_SIGNAL_CALIBRATION") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => std::env::var("MSR_FULL").ok().as_deref() == Some("1"),
    }
}

pub(super) fn msr_signal_sample_outputs(
    outputs: &[MsrOutputSpec],
    limit: usize,
) -> Vec<MsrOutputSpec> {
    if limit == 0 || outputs.is_empty() {
        return Vec::new();
    }
    let mut indexes = Vec::new();
    fn push_unique(indexes: &mut Vec<usize>, output_count: usize, index: usize) {
        if index < output_count && !indexes.contains(&index) {
            indexes.push(index);
        }
    }

    push_unique(&mut indexes, outputs.len(), 0);
    if let Some(index) = outputs
        .iter()
        .position(|output| output.protocol == MsrProtocol::Srt)
    {
        push_unique(&mut indexes, outputs.len(), index);
    }
    push_unique(&mut indexes, outputs.len(), outputs.len() / 2);
    push_unique(&mut indexes, outputs.len(), outputs.len().saturating_sub(1));
    if indexes.len() < limit {
        for index in (0..outputs.len()).step_by((outputs.len() / limit.max(1)).max(1)) {
            push_unique(&mut indexes, outputs.len(), index);
            if indexes.len() >= limit {
                break;
            }
        }
    }
    indexes
        .into_iter()
        .take(limit)
        .map(|index| outputs[index].clone())
        .collect()
}

async fn run_msr_signal_check(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    output: &MsrOutputSpec,
    duration: u64,
) -> Result<Value, String> {
    let url = msr_read_url(env, output);
    let label = format!("{}-{}-{}", MSR_MODE, checkpoint, output.name);
    let stem = safe_artifact_stem(&label);
    let capture_path = env.work_dir.join(format!("{stem}.signal.mkv"));
    let blackdetect_log = env.work_dir.join(format!("{stem}.blackdetect.log"));
    let silencedetect_log = env.work_dir.join(format!("{stem}.silencedetect.log"));
    let ashowinfo_log = env.work_dir.join(format!("{stem}.ashowinfo.log"));
    let astats_log = env.work_dir.join(format!("{stem}.astats.log"));

    capture_signal_sample(&url, &capture_path, duration).await?;
    let black = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &[
            "-vf",
            "blackdetect=d=0.05:pix_th=0.10",
            "-an",
            "-f",
            "null",
            "-",
        ],
        &blackdetect_log,
    )
    .await?;
    let silence = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &[
            "-af",
            "silencedetect=n=-35dB:d=0.05",
            "-vn",
            "-f",
            "null",
            "-",
        ],
        &silencedetect_log,
    )
    .await?;
    let ashow = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &["-af", "ashowinfo", "-vn", "-f", "null", "-"],
        &ashowinfo_log,
    )
    .await?;
    let astats = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &["-af", "astats=metadata=1:reset=1", "-vn", "-f", "null", "-"],
        &astats_log,
    )
    .await?;
    let pcm = decode_pcm_quality(&capture_path, duration).await?;
    let report = validate_signal_quality_with_tolerances(
        &black,
        &silence,
        &ashow,
        &astats,
        pcm,
        &SignalTolerances::default(),
    )
    .map_err(|error| format!("{} signal validation failed: {error}", output.name))?;
    Ok(json!({
        "kind": "msrSignalQuality",
        "mode": MSR_MODE,
        "checkpoint": checkpoint,
        "output": {
            "name": output.name,
            "ordinal": output.ordinal,
            "rank": output.rank,
            "protocol": output.protocol.label(),
            "rtmpMode": output.rtmp_mode_name(),
            "encoding": output.encoding,
        },
        "quality": signal_report_json(
            &label,
            &url,
            duration,
            &capture_path,
            &blackdetect_log,
            &silencedetect_log,
            &ashowinfo_log,
            &astats_log,
            &report,
        ),
    }))
}

pub(super) async fn run_msr_signal_checkpoint(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    outputs: &[MsrOutputSpec],
) -> Result<Vec<Value>, String> {
    let sample_limit = env_usize("MSR_SIGNAL_SAMPLES_PER_CHECKPOINT", 4);
    let duration = env_secs("MSR_SIGNAL_SAMPLE_SECS", 20);
    let mut checks = Vec::new();
    for output in msr_signal_sample_outputs(outputs, sample_limit) {
        let check = run_msr_signal_check(env, checkpoint, &output, duration).await?;
        append_line(
            &env.samples_jsonl,
            &format!("{}\n", serde_json::to_string(&check).unwrap()),
        )?;
        checks.push(check);
    }
    Ok(checks)
}
