use super::*;

fn path_body(online: bool, source: &str, codecs: &[&str]) -> Value {
    json!({
        "name": "live",
        "online": online,
        "source": {"type": source, "id": "x"},
        "tracks2": codecs
            .iter()
            .map(|codec| json!({"codec": codec, "codecProps": {}}))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn a_relay_path_counts_as_published_only_online_from_the_named_source_with_both_tracks() {
    let both = [RELAY_VIDEO_LABEL, RELAY_AUDIO_LABEL];
    assert!(relay_path_published(
        &path_body(true, "moqSession", &both),
        "moqSession"
    ));
    // Offline, or published by something other than Restream's MoQ session.
    assert!(!relay_path_published(
        &path_body(false, "moqSession", &both),
        "moqSession"
    ));
    assert!(!relay_path_published(
        &path_body(true, "rtmpConn", &both),
        "moqSession"
    ));
    // A missing track.
    assert!(!relay_path_published(
        &path_body(true, "moqSession", &[RELAY_VIDEO_LABEL]),
        "moqSession"
    ));
    assert!(!relay_path_published(
        &path_body(true, "moqSession", &[RELAY_AUDIO_LABEL]),
        "moqSession"
    ));
    // No source at all (the path exists but nobody publishes).
    assert!(!relay_path_published(
        &json!({"online": true, "source": null, "tracks2": []}),
        "moqSession"
    ));
}

#[test]
fn frame_rates_parse_as_ffprobe_writes_them() {
    assert_eq!(parse_frame_rate("30/1"), Some(30.0));
    let ntsc = parse_frame_rate("30000/1001").expect("ntsc rate");
    assert!((ntsc - 29.97).abs() < 0.001);
    assert_eq!(parse_frame_rate("0/0"), None);
    assert_eq!(parse_frame_rate("30"), None);
    assert_eq!(parse_frame_rate("x/1"), None);
}

/// An FFprobe body: stream 0 H.264 video, stream 1 AAC audio, packets
/// interleaved by time.
fn probe_body(video_pts: &[f64], audio_pts: &[f64]) -> Value {
    let mut packets: Vec<(f64, u64)> = video_pts
        .iter()
        .map(|pts| (*pts, 0))
        .chain(audio_pts.iter().map(|pts| (*pts, 1)))
        .collect();
    packets.sort_by(|a, b| a.0.total_cmp(&b.0));
    json!({
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "h264"},
            {"index": 1, "codec_type": "audio", "codec_name": "aac"},
        ],
        "packets": packets
            .iter()
            .map(|(pts, index)| json!({"stream_index": index, "pts_time": format!("{pts:.6}")}))
            .collect::<Vec<_>>(),
    })
}

fn series(count: usize, step: f64) -> Vec<f64> {
    (0..count).map(|i| i as f64 * step).collect()
}

#[test]
fn a_clean_twenty_second_read_passes_every_relay_check() {
    // 20 s at 30 fps is 601 frames counting both ends.
    let probe = probe_body(&series(601, 1.0 / 30.0), &series(939, 1024.0 / 48_000.0));
    let sample = RelaySample::from_probe(&probe);
    assert_eq!(sample.video_codec.as_deref(), Some("h264"));
    assert_eq!(sample.audio_codec.as_deref(), Some("aac"));
    assert_eq!(sample.video_frames, 601);
    assert!((sample.video_span_secs - 20.0).abs() < 1e-3);
    assert!(
        sample.failures(30.0, 20.0).is_empty(),
        "{:?}",
        sample.failures(30.0, 20.0)
    );
}

#[test]
fn a_read_shorter_than_the_window_fails() {
    let probe = probe_body(&series(541, 1.0 / 30.0), &series(845, 1024.0 / 48_000.0));
    let failures = RelaySample::from_probe(&probe).failures(30.0, 20.0);
    assert!(
        failures.iter().any(|f| f.starts_with("video spans")),
        "{failures:?}"
    );
    assert!(
        failures.iter().any(|f| f.starts_with("audio spans")),
        "{failures:?}"
    );
}

#[test]
fn the_frame_count_must_be_within_five_percent_of_the_input_rate() {
    let audio = series(939, 1024.0 / 48_000.0);
    // 20 s of media but only every other frame: half the input rate.
    let halved = probe_body(&series(301, 2.0 / 30.0), &audio);
    let failures = RelaySample::from_probe(&halved).failures(30.0, 20.0);
    assert!(
        failures.iter().any(|f| f.contains("video frames over")),
        "{failures:?}"
    );

    // Six frames short over 20 s (1 %) is inside the tolerance.
    let mut slightly_short = series(601, 1.0 / 30.0);
    for index in [100, 200, 300, 400, 500, 550] {
        slightly_short[index] = f64::NAN;
    }
    slightly_short.retain(|pts| !pts.is_nan());
    let sample = RelaySample::from_probe(&probe_body(&slightly_short, &audio));
    assert!(sample.failures(30.0, 20.0).is_empty());

    // Forty frames short (6.7 %) is not.
    let mut short = series(601, 1.0 / 30.0);
    short.drain(100..140);
    let failures = RelaySample::from_probe(&probe_body(&short, &audio)).failures(30.0, 20.0);
    assert!(
        failures.iter().any(|f| f.contains("video frames over")),
        "{failures:?}"
    );
}

#[test]
fn a_repeated_or_backwards_video_pts_fails_but_a_repeated_audio_pts_does_not() {
    let audio = series(939, 1024.0 / 48_000.0);
    let mut video = series(601, 1.0 / 30.0);
    video[300] = video[299];
    let failures = RelaySample::from_probe(&probe_body(&video, &audio)).failures(30.0, 20.0);
    assert!(failures.contains(&"video PTS not strictly increasing".to_string()));

    // FFmpeg's RTP depacketizer stamps every AU of an aggregated packet with
    // the packet's timestamp: equal audio PTS values are expected.
    let mut aggregated = audio.clone();
    aggregated[11] = aggregated[10];
    let sample = RelaySample::from_probe(&probe_body(&series(601, 1.0 / 30.0), &aggregated));
    assert!(sample.audio_pts_nondecreasing);

    // A step back is not.
    let mut backwards = audio;
    backwards[500] = backwards[400];
    // The body is sorted by time, so build it by hand to keep demux order.
    let mut probe = probe_body(&series(601, 1.0 / 30.0), &[]);
    let packets = probe["packets"].as_array_mut().expect("packets");
    packets.extend(
        backwards
            .iter()
            .map(|pts| json!({"stream_index": 1, "pts_time": format!("{pts:.6}")})),
    );
    let failures = RelaySample::from_probe(&probe).failures(30.0, 20.0);
    assert!(failures.contains(&"audio PTS went backwards or is missing".to_string()));
}

#[test]
fn a_missing_track_codec_or_timestamp_fails() {
    let video_only = json!({
        "streams": [{"index": 0, "codec_type": "video", "codec_name": "h264"}],
        "packets": series(601, 1.0 / 30.0)
            .iter()
            .map(|pts| json!({"stream_index": 0, "pts_time": format!("{pts:.6}")}))
            .collect::<Vec<_>>(),
    });
    let failures = RelaySample::from_probe(&video_only).failures(30.0, 20.0);
    assert!(
        failures.iter().any(|f| f.starts_with("audio codec")),
        "{failures:?}"
    );

    let mut hevc = probe_body(&series(601, 1.0 / 30.0), &series(939, 1024.0 / 48_000.0));
    hevc["streams"][0]["codec_name"] = json!("hevc");
    let failures = RelaySample::from_probe(&hevc).failures(30.0, 20.0);
    assert!(
        failures.iter().any(|f| f.starts_with("video codec")),
        "{failures:?}"
    );

    let mut no_pts = probe_body(&series(601, 1.0 / 30.0), &series(939, 1024.0 / 48_000.0));
    no_pts["packets"][5]["pts_time"] = json!("N/A");
    let sample = RelaySample::from_probe(&no_pts);
    assert!(!sample.video_pts_increasing || !sample.audio_pts_nondecreasing);
}

#[test]
fn the_longest_stall_includes_a_counter_that_stops_for_good() {
    let ms = Duration::from_millis;
    let steady: Vec<(Duration, u64)> = (0..50).map(|i| (ms(i * 100), i * 3)).collect();
    assert_eq!(longest_counter_stall(&steady), ms(100));

    // Stuck from 1.0 s to 2.5 s, then moving again.
    let mut middle = steady.clone();
    for sample in middle
        .iter_mut()
        .filter(|(at, _)| *at > ms(1000) && *at < ms(2500))
    {
        sample.1 = 30;
    }
    for sample in middle.iter_mut().filter(|(at, _)| *at >= ms(2500)) {
        sample.1 += 1000;
    }
    assert_eq!(longest_counter_stall(&middle), ms(1500));

    // Stuck from 4.0 s to the last sample at 4.9 s: the tail counts.
    let mut tail = steady;
    for sample in tail.iter_mut().filter(|(at, _)| *at > ms(4000)) {
        sample.1 = 120;
    }
    assert_eq!(longest_counter_stall(&tail), ms(900));

    assert_eq!(longest_counter_stall(&[]), Duration::ZERO);
    assert_eq!(longest_counter_stall(&[(ms(5), 1)]), Duration::ZERO);
}

#[test]
fn the_certificate_fingerprint_is_lowercase_hex_as_mediamtx_compares_it() {
    assert_eq!(
        certificate_sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn moq_relay_interop_is_a_catalog_command_off_the_default_suite() {
    let spec = mode_spec("moq-relay-interop").expect("moq-relay-interop must be in modes.json");
    assert!(
        spec.requires_port_namespace,
        "it binds harness ports and must stay isolated by default"
    );
    assert!(
        !spec.requires_bench_profile,
        "a correctness interop check, not a measurement"
    );
    assert!(
        !spec.suite_default,
        "it needs the pinned MediaMTX, which the default suite does not install"
    );
    assert!(
        supported_mode_names()
            .iter()
            .any(|name| name == "moq-relay-interop")
    );
}
