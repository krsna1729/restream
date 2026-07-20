proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn proptest_stage_args_probe_flags_follow_startup_policy(
        preset_case in 0u8..12,
        input_codec_is_hevc in any::<bool>(),
        probe_codec_is_hevc in any::<bool>(),
        include_audio in any::<bool>(),
        audio_track_count in 0usize..=64,
        observed_bitrate_bps in prop::option::of(0u64..=80_000_000),
    ) {
        let input_codec = if input_codec_is_hevc { "hevc" } else { "h264" };
        let probe_codec = if probe_codec_is_hevc { "hevc" } else { "h264" };
        let preset = preset_for_probe_property(preset_case, audio_track_count);
        let args = build_stage_ffmpeg_args_for_observed_input_streams(
            &preset,
            input_codec,
            probe_codec,
            include_audio,
            audio_track_count,
            observed_bitrate_bps,
        );

        let stage_spec = StagePresetSpec::parse(&preset);
        let audio_routing = stage_audio_routing(&preset);
        let full_stream_passthrough =
            matches!(stage_spec.video_encoding(), "source" | "") && audio_routing.is_none();
        let probed_audio_track_count =
            probe_audio_track_count(&audio_routing, include_audio, audio_track_count);
        let (expected_analyze_duration_us, expected_probe_size_bytes) =
            startup_policy::ext_stage_probe_budget_for(startup_policy::ExtStageProbeContext {
                codec: VideoCodecKind::from_codec_name(probe_codec),
                include_audio,
                audio_track_count: probed_audio_track_count,
                passthrough: full_stream_passthrough,
                observed_bitrate_bps,
            });

        prop_assert_eq!(
            arg_after(&args, "-analyzeduration"),
            expected_analyze_duration_us.to_string()
        );
        prop_assert_eq!(
            arg_after(&args, "-probesize"),
            expected_probe_size_bytes.to_string()
        );
    }
}

#[test]
fn stage_args_720p_reads_stdin_writes_stdout() {
    let args = build_stage_ffmpeg_args("720p", "h264");
    assert!(args.windows(2).any(|w| w == ["-threads", "2"]));
    assert!(args.iter().any(|a| a == "-i"));
    let i_pos = args.iter().position(|a| a == "-i").unwrap();
    assert_eq!(args[i_pos + 1], "pipe:0");
    assert!(args.iter().any(|a| a == "-vf"));
    let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
    assert!(args[vf_pos + 1].contains("1280"));
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "libx264");
    assert!(args.windows(2).any(|w| w == ["-flush_packets", "1"]));
    assert!(args.windows(2).any(|w| w == ["-muxdelay", "0"]));
    assert!(args.windows(2).any(|w| w == ["-muxpreload", "0"]));
    assert!(args.windows(2).any(|w| w == ["-pes_payload_size", "0"]));
    let (analyze_duration_us, probe_size_bytes) =
        startup_policy::ext_stage_probe_budget(VideoCodecKind::H264);
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
    );
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
    );
    assert!(args.last() == Some(&"pipe:1".to_string()));
}

#[test]
fn external_stderr_filter_drops_expected_hevc_decoder_chatter() {
    let text = concat!(
        "[hevc @ 0x1] Could not find ref with POC 512\n",
        "[hevc @ 0x1] Error constructing the frame RPS.\n",
        "[hevc @ 0x1] Skipping invalid undecodable NALU: 1\n"
    );

    assert!(actionable_external_ffmpeg_stderr(text).is_empty());
}

#[test]
fn external_stderr_filter_keeps_actionable_lines() {
    let text = concat!(
        "[hevc @ 0x1] Could not find ref with POC 512\n",
        "Conversion failed!\n"
    );

    assert_eq!(
        actionable_external_ffmpeg_stderr(text),
        "Conversion failed!"
    );
}

#[test]
fn stage_args_hevc_raise_probe_budget() {
    let args = build_stage_ffmpeg_args("720p", "hevc");
    let (analyze_duration_us, probe_size_bytes) =
        startup_policy::ext_stage_probe_budget(VideoCodecKind::Hevc);
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
    );
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
    );
}

#[test]
fn stage_args_codec_edge_probes_input_codec_but_encodes_output_codec() {
    let args = build_stage_ffmpeg_args_for_input("h264", "h264", "hevc");
    let (analyze_duration_us, probe_size_bytes) =
        startup_policy::ext_stage_probe_budget(VideoCodecKind::Hevc);
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "libx264");
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
    );
    assert!(
        args.windows(2)
            .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
    );
}

#[test]
fn stage_args_scale_probe_budget_by_observed_audio_streams() {
    for (codec, tracks, expected_probe_size) in [
        ("h264", 1, 128 * 1024),
        ("h264", 10, 272 * 1024),
        ("h264", 30, 592 * 1024),
        ("hevc", 1, 512 * 1024),
        ("hevc", 10, 656 * 1024),
        ("hevc", 30, 976 * 1024),
    ] {
        let args =
            build_stage_ffmpeg_args_for_input_streams("720p", codec, codec, true, tracks);
        assert_eq!(
            arg_after(&args, "-probesize"),
            expected_probe_size.to_string(),
            "codec={codec} tracks={tracks}"
        );
    }

    let args_video_only =
        build_stage_ffmpeg_args_for_input_streams("720p", "h264", "h264", false, 30);

    assert_eq!(
        arg_after(&args_video_only, "-probesize"),
        (128 * 1024).to_string()
    );
}

#[test]
fn stage_args_probe_budget_covers_common_output_resolutions() {
    let _guard = profile_cache_test_lock();
    {
        let mut cache = crate::media::profiles::cache().blocking_write();
        for (name, width, height) in [
            ("240p_test", 426, 240),
            ("480p_test", 854, 480),
            ("4k_test", 3840, 2160),
        ] {
            cache.insert(
                name.to_string(),
                TranscodeProfile {
                    preset: "ultrafast".to_string(),
                    tune: "zerolatency".to_string(),
                    crf: 23,
                    gop: 60,
                    bframes: 0,
                    bitrate: 0,
                    max_bitrate: 0,
                    width,
                    height,
                },
            );
        }
    }

    for preset in ["240p_test", "480p_test", "720p", "1080p", "4k_test"] {
        let args = build_stage_ffmpeg_args_for_input_streams(preset, "h264", "h264", true, 1);
        assert_eq!(
            arg_after(&args, "-probesize"),
            (128 * 1024).to_string(),
            "preset={preset}"
        );
        assert!(
            args.iter().any(|arg| arg.starts_with("scale=")),
            "preset={preset}"
        );
    }
}

#[test]
fn stage_args_hevc_multi_audio_probe_stays_bounded() {
    let args = build_stage_ffmpeg_args_for_input_streams("h264", "h264", "hevc", true, 30);
    assert_eq!(arg_after(&args, "-probesize"), (976 * 1024).to_string());
    assert_eq!(arg_after(&args, "-analyzeduration"), "1000000");
}

#[test]
fn complex_audio_args_probe_only_referenced_input_tracks() {
    let downmix_track0 =
        build_stage_ffmpeg_args_for_input_streams("downmix:0", "h264", "h264", true, 30);
    let remap_track9 =
        build_stage_ffmpeg_args_for_input_streams("remap:0:1:9", "h264", "h264", true, 30);
    let hevc_downmix_track29 =
        build_stage_ffmpeg_args_for_input_streams("downmix:29", "h264", "hevc", true, 30);

    assert_eq!(
        arg_after(&downmix_track0, "-probesize"),
        (128 * 1024).to_string()
    );
    assert_eq!(
        arg_after(&remap_track9, "-probesize"),
        (272 * 1024).to_string()
    );
    assert_eq!(
        arg_after(&hevc_downmix_track29, "-probesize"),
        (976 * 1024).to_string()
    );
}

#[test]
fn stage_args_720p_hevc_uses_libx265() {
    for codec in &["hevc", "h265"] {
        let args = build_stage_ffmpeg_args("720p", codec);
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "libx265", "codec={codec}");
        let x265_pos = args.iter().position(|a| a == "-x265-params").unwrap();
        assert_eq!(args[x265_pos + 1], "repeat-headers=1:log-level=none");
        assert!(args.last() == Some(&"pipe:1".to_string()));
    }
}

#[test]
fn stage_args_custom_profile_uses_profile_settings() {
    let _guard = profile_cache_test_lock();
    {
        let mut cache = crate::media::profiles::cache().blocking_write();
        cache.insert(
            "square_test".to_string(),
            TranscodeProfile {
                preset: "superfast".to_string(),
                tune: "zerolatency".to_string(),
                crf: 21,
                gop: 100,
                bframes: 1,
                bitrate: 1500000,
                max_bitrate: 2000000,
                width: 640,
                height: 640,
            },
        );
    }

    let args = build_stage_ffmpeg_args("square_test", "h264");
    assert!(args.windows(2).any(|w| w == ["-vf", "scale=640:640"]));
    assert!(args.windows(2).any(|w| w == ["-preset", "superfast"]));
    assert!(args.windows(2).any(|w| w == ["-g", "100"]));
    assert!(args.windows(2).any(|w| w == ["-bf", "1"]));
    assert!(args.windows(2).any(|w| w == ["-b:v", "1500000"]));
    assert!(args.windows(2).any(|w| w == ["-maxrate", "2000000"]));
    assert!(!args.iter().any(|arg| arg == "-crf"));
}

#[test]
fn stage_args_source_copies_video() {
    let args = build_stage_ffmpeg_args("source", "h264");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
    assert!(!args.iter().any(|a| a == "-vf"));
    assert!(args.last() == Some(&"pipe:1".to_string()));
}

#[test]
fn stage_args_h264_transcodes_without_scaling() {
    let args = build_stage_ffmpeg_args("h264", "h264");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "libx264");
    assert!(!args.iter().any(|a| a == "-vf"));
}

#[test]
fn stage_args_video_prefix_stripped() {
    let a = build_stage_ffmpeg_args("video:720p", "h264");
    let b = build_stage_ffmpeg_args("720p", "h264");
    assert_eq!(a, b);
}

#[test]
fn stage_args_non_dsp_audio_is_copied() {
    for preset in &["720p", "1080p", "source"] {
        let args = build_stage_ffmpeg_args(preset, "h264");
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "copy", "preset={preset}");
    }
}

#[test]
fn stage_args_remap_uses_pan_filter_and_audio_encode() {
    let args = build_stage_ffmpeg_args("audio:remap:1:0:2:from:720p", "h264");

    let filter_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    assert_eq!(args[filter_pos + 1], "[0:a:2]pan=stereo|c0=c1|c1=c0[aout]");
    assert!(args.windows(2).any(|w| w == ["-map", "0:v:0?"]));
    assert!(args.windows(2).any(|w| w == ["-map", "[aout]"]));
    let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
    assert_eq!(args[ca_pos + 1], "aac");
    assert!(args.windows(2).any(|w| w == ["-ac", "2"]));
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
}

#[test]
fn stage_args_downmix_uses_stereo_resample_filter() {
    let args = build_stage_ffmpeg_args("audio:downmix:1:from:source", "h264");

    let filter_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    assert_eq!(
        args[filter_pos + 1],
        "[0:a:1]aresample=out_chlayout=stereo[aout]"
    );
    let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
    assert_eq!(args[ca_pos + 1], "aac");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
}

#[test]
fn stage_args_atrack_stays_packet_copy() {
    let args = build_stage_ffmpeg_args("audio:atrack:0:from:720p", "h264");

    assert!(!args.iter().any(|a| a == "-filter_complex"));
    let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
    assert_eq!(args[ca_pos + 1], "copy");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
}

#[test]
fn stage_args_empty_preset_copies_video_and_audio() {
    let args = build_stage_ffmpeg_args("", "h264");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
    let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
    assert_eq!(args[ca_pos + 1], "copy");
}

#[test]
fn stage_args_custom_preset_copies_video_and_audio() {
    let args = build_stage_ffmpeg_args("custom", "h264");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
    let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
    assert_eq!(args[ca_pos + 1], "copy");
}

#[test]
fn stage_audio_routing_remap_is_some() {
    let r = stage_audio_routing("audio:remap:0:1:0:from:source");
    assert!(r.is_some());
    assert!(matches!(r, Some(AudioRouting::Remap { .. })));
}

#[test]
fn stage_audio_routing_downmix_is_some() {
    let r = stage_audio_routing("audio:downmix:0:from:source");
    assert!(r.is_some());
    assert!(matches!(r, Some(AudioRouting::Downmix { .. })));
}

#[test]
fn stage_audio_routing_atrack_returns_none() {
    let r = stage_audio_routing("audio:atrack:0:from:720p");
    assert!(r.is_none());
}

#[test]
fn stage_audio_routing_video_preset_returns_none() {
    assert!(stage_audio_routing("720p").is_none());
    assert!(stage_audio_routing("source").is_none());
}

#[test]
fn audio_filter_complex_remap_format() {
    let routing = Some(AudioRouting::Remap {
        left: 1,
        right: 0,
        track: 2,
    });
    let filter = audio_filter_complex(&routing).unwrap();
    assert_eq!(filter, "[0:a:2]pan=stereo|c0=c1|c1=c0[aout]");
}

#[test]
fn audio_filter_complex_downmix_format() {
    let routing = Some(AudioRouting::Downmix { track: 1 });
    let filter = audio_filter_complex(&routing).unwrap();
    assert_eq!(filter, "[0:a:1]aresample=out_chlayout=stereo[aout]");
}

#[test]
fn audio_filter_complex_none_for_no_routing() {
    assert!(audio_filter_complex(&None).is_none());
}

#[test]
fn stage_args_profile_with_crf_when_bitrate_zero() {
    let _guard = profile_cache_test_lock();
    {
        let mut cache = crate::media::profiles::cache().blocking_write();
        cache.insert(
            "crf_test".to_string(),
            TranscodeProfile {
                preset: "veryfast".to_string(),
                tune: String::new(),
                crf: 28,
                gop: 60,
                bframes: 0,
                bitrate: 0,
                max_bitrate: 0,
                width: 1280,
                height: 720,
            },
        );
    }
    let args = build_stage_ffmpeg_args("crf_test", "h264");
    assert!(args.windows(2).any(|w| w == ["-crf", "28"]));
    assert!(!args.iter().any(|a| a == "-b:v"));
    assert!(!args.iter().any(|a| a == "-maxrate"));
}

#[test]
fn stage_args_audio_stage_strips_prefix_and_copies_video() {
    let args = build_stage_ffmpeg_args("audio:atrack:0:from:720p", "h264");
    let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
    assert_eq!(args[cv_pos + 1], "copy");
    assert!(!args.iter().any(|a| a == "-vf"));
}

#[tokio::test]
async fn kill_and_wait_on_child_without_piped_stdin_does_not_hang() {
    let mut child = tokio::process::Command::new("true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn 'true'");

    assert!(child.stdin.take().is_none());

    let _ = child.kill().await;
    let status = child.wait().await.expect("wait must not fail");
    let _ = status;
}
