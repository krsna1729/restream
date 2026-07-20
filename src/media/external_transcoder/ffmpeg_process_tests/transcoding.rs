#[test]
fn feeder_remuxed_single_audio_hevc_fixture_transcodes_as_file_input() {
    let (video, audio_tracks, packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();

    for packet in &packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }

    let input_path = write_temp_ts_artifact("hevc-feeder-transcode-input", &ts_bytes);
    let output_path = input_path
        .parent()
        .expect("temp artifact dir")
        .join("output.ts");
    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let mut args = build_stage_ffmpeg_args_for_input("720p", "h264", "hevc");
    let input_pos = args
        .iter()
        .position(|arg| arg == "-i")
        .expect("stage args should contain input flag");
    args[input_pos + 1] = input_path.to_string_lossy().to_string();
    let last = args.last_mut().expect("stage args should contain output");
    *last = output_path.to_string_lossy().to_string();

    let output = std::process::Command::new(ffmpeg)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn bundled ffmpeg transcode");

    assert!(
        output.status.success(),
        "ffmpeg should transcode feeder-remuxed HEVC TS file input: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::metadata(&output_path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false),
        "file-based transcode should produce a non-empty TS output"
    );
}

#[test]
fn feeder_remuxed_h264_marker_fixture_transcodes_as_file_input() {
    let path =
        crate::test_fixtures::av_marker_transport_fixture("h264", false).expect("marker path");
    let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
    let video = probe.video.expect("marker fixture should contain video");
    let audio_tracks = probe.audio_tracks;

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();

    for packet in &packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }

    assert!(
        !ts_bytes.is_empty(),
        "remuxed H.264 marker fixture should produce TS bytes"
    );

    let input_path = write_temp_ts_artifact("h264-marker-transcode-input", &ts_bytes);
    let output_path = input_path
        .parent()
        .expect("temp artifact dir")
        .join("output.ts");
    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let mut args = build_stage_ffmpeg_args("720p", "h264");
    let input_pos = args
        .iter()
        .position(|arg| arg == "-i")
        .expect("stage args should contain input flag");
    args[input_pos + 1] = input_path.to_string_lossy().to_string();
    let last = args.last_mut().expect("stage args should contain output");
    *last = output_path.to_string_lossy().to_string();

    let output = std::process::Command::new(ffmpeg)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn bundled ffmpeg transcode");

    assert!(
        output.status.success(),
        "ffmpeg should transcode feeder-remuxed H.264 marker TS file input: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::metadata(&output_path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false),
        "file-based marker transcode should produce a non-empty TS output"
    );

    let video_only_path = input_path
        .parent()
        .expect("temp artifact dir")
        .join("output-video-only.ts");
    let decode_video = std::process::Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-i",
            output_path.to_string_lossy().as_ref(),
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-f",
            "mpegts",
            video_only_path.to_string_lossy().as_ref(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("probe transcoded marker TS video stream");
    assert!(
        decode_video.status.success(),
        "transcoded marker output should contain a decodable video stream: {}",
        String::from_utf8_lossy(&decode_video.stderr)
    );
}

#[test]
fn feeder_remuxed_h264_marker_fixture_transcodes_as_live_pipe_input() {
    let path = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h264",
        false,
        crate::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("marker path");
    let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
    let video = probe.video.expect("marker fixture should contain video");
    let audio_tracks = probe.audio_tracks;

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();

    for _ in 0..4 {
        for packet in &packets {
            packet_buf.clear();
            if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
                ts_bytes.extend_from_slice(&packet_buf);
            }
        }
    }

    assert!(
        !ts_bytes.is_empty(),
        "remuxed H.264 marker fixture should produce TS bytes"
    );

    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let mut child = std::process::Command::new(ffmpeg)
        .args(build_stage_ffmpeg_args("720p", "h264"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bundled ffmpeg transcode");

    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 188 * 16];
        if let Ok(n) = std::io::Read::read(&mut stdout, &mut buf) {
            let _ = tx.send(n);
        }
    });

    let writer = std::thread::spawn(move || {
        for chunk in ts_bytes.chunks(1316) {
            if std::io::Write::write_all(&mut stdin, chunk).is_err() {
                break;
            }
        }
        stdin
    });

    let live_bytes = match rx.recv_timeout(std::time::Duration::from_secs(12)) {
        Ok(n) => n,
        Err(err) => {
            let mut stdin = writer.join().expect("join writer");
            let _ = std::io::Write::flush(&mut stdin);
            drop(stdin);
            let _ = child.kill();
            let output = child.wait_with_output().expect("wait for ffmpeg");
            let _ = reader.join();
            panic!(
                "ffmpeg should emit stdout before stdin closes: {err}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert!(live_bytes > 0, "ffmpeg stdout should not be empty");

    let mut stdin = writer.join().expect("join writer");
    let _ = std::io::Write::flush(&mut stdin);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}

#[test]
fn observed_rate_probe_transcodes_h264_aac_bitrate_fixture_before_pipe_closes() {
    // Regression: the 1.5 Mbps SRT bitrate-sweep row failed when a fixed
    // 128 KiB probe ended inside the leading video burst, before FFmpeg
    // had parsed AAC channel/sample-rate parameters. Derive the same rate
    // Restream observes from retained packets and keep stdin open while
    // proving that the dynamic budget emits output.
    let path = crate::test_fixtures::bench_transport_fixture("h264", "1.5M", false)
        .expect("1.5 Mbps H.264/AAC fixture");
    let file_bytes = std::fs::read(&path).expect("read bitrate fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let mut probe = demuxer.take_probe().expect("probe bitrate fixture");
    let video = probe.video.take().expect("H.264 video metadata");
    let audio_tracks = probe.audio_tracks;
    // Exercise the production estimator rather than duplicating its math
    // in the regression. Otherwise the stage wiring could fall back to a
    // fixed probe size while this test continued to pass independently.
    let observed_ring = crate::media::ring_buffer::RingBuffer::new(packets.len().max(2));
    for packet in &packets {
        observed_ring.push(packet.clone());
    }
    let observed_bitrate_bps = observed_ring
        .observed_payload_bitrate_bps()
        .expect("fixture provides a sufficient retained media window");

    let args = build_stage_ffmpeg_args_for_observed_input_streams(
        "720p",
        "h264",
        "h264",
        true,
        audio_tracks.len(),
        Some(observed_bitrate_bps),
    );
    let probe_size: usize = arg_after(&args, "-probesize").parse().unwrap();
    assert!(
        observed_bitrate_bps >= 4_000_000,
        "fixture must exercise the burst-tolerant estimator, not only the whole-window average"
    );
    assert!(
        probe_size > 128 * 1024,
        "observed bitrate must lift the failed 128 KiB probe floor"
    );
    assert!(
        probe_size >= 900 * 1024,
        "fixture burst must receive enough probe room to cover early AAC/video headers"
    );
    assert!(
        probe_size <= 2 * 1024 * 1024,
        "live probe must remain under the global startup cap"
    );

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();
    for packet in &packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }

    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let mut child = std::process::Command::new(ffmpeg)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bundled dynamic-probe transcode");
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 188 * 16];
        if let Ok(n) = std::io::Read::read(&mut stdout, &mut buf) {
            let _ = tx.send(n);
        }
    });
    let writer = std::thread::spawn(move || {
        for chunk in ts_bytes.chunks(1316) {
            if std::io::Write::write_all(&mut stdin, chunk).is_err() {
                break;
            }
        }
        stdin
    });

    let live_bytes = match rx.recv_timeout(std::time::Duration::from_secs(12)) {
        Ok(n) => n,
        Err(err) => {
            let mut stdin = writer.join().expect("join writer");
            let _ = std::io::Write::flush(&mut stdin);
            drop(stdin);
            let _ = child.kill();
            let output = child.wait_with_output().expect("wait for ffmpeg");
            let _ = reader.join();
            panic!(
                "dynamic probe should emit before stdin closes: {err}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert!(live_bytes > 0, "dynamic-probe stdout should not be empty");

    let mut stdin = writer.join().expect("join writer");
    let _ = std::io::Write::flush(&mut stdin);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}

#[test]
fn feeder_remuxed_hevc_fixture_transcodes_before_live_pipe_closes() {
    // Regression: a July 2026 H.264 startup tuning pass proved only AVC.
    // HEVC SRT sources can keep stdin open indefinitely, so waiting for EOF
    // before stdout appears leaves every downstream output in
    // `waitingUpstream`. Keep this pipe-open proof beside the AVC one: the
    // probe and mux settings must produce H.264 bytes while HEVC is live.
    let path = crate::test_fixtures::canonical_ts_fixture("h265")
        .expect("single-audio HEVC fixture path");
    let file_bytes = std::fs::read(&path).expect("read HEVC fixture");
    let mut demuxer = TsDemuxer::new();
    let mut all_packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut all_packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut all_packets);
    let mut probe = demuxer.take_probe().expect("probe HEVC fixture");
    let video = probe.video.take().expect("HEVC video metadata");
    let mut audio_tracks: Vec<_> = probe.audio_tracks.drain(..).take(1).collect();
    let source_audio_track = audio_tracks
        .first()
        .map(|track| track.track_index)
        .expect("HEVC fixture audio metadata");
    audio_tracks[0].track_index = 0;
    // Retain transport order. A live SRT ring interleaves video and audio;
    // grouping every video packet first makes FFmpeg wait for AAC
    // parameters that production already supplied and hides the real
    // persistent-pipe startup behavior.
    let packets: Vec<_> = all_packets
        .into_iter()
        .filter_map(|mut packet| match packet.media_type {
            crate::media::packet::MediaType::Video => Some(packet),
            crate::media::packet::MediaType::Audio
                if packet.track_index == source_audio_track =>
            {
                packet.track_index = 0;
                Some(packet)
            }
            _ => None,
        })
        .collect();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let parameter_sets = packets
        .iter()
        .find_map(|packet| {
            (packet.media_type == crate::media::packet::MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        })
        .expect("HEVC fixture parameter sets");
    feeder.set_raw_video_parameter_sets_if_empty(&parameter_sets);

    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();
    // One fixture pass represents the sparse live start seen from SRT. Do
    // not repeat it until FFmpeg crosses the probe ceiling: that hides the
    // exact regression where every stage remained at `firstInput` while a
    // low-bitrate HEVC publisher kept its pipe open.
    // A complete HEVC + AAC live-start window fits in 640 KiB. This is
    // above the headers required for both streams, but far below the old
    // 2 MiB probe ceiling that caused the SRT harness stall.
    const LIVE_STARTUP_TS_BUDGET: usize = 640 * 1024;
    for packet in &packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            if ts_bytes.len() + packet_buf.len() > LIVE_STARTUP_TS_BUDGET {
                break;
            }
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }
    assert!(
        !ts_bytes.is_empty(),
        "HEVC fixture should remux to TS bytes"
    );
    assert!(
        ts_bytes.len() <= LIVE_STARTUP_TS_BUDGET,
        "the live-start regression fixture must stay below the old 2 MiB probe ceiling"
    );
    const LIVE_STARTUP_BATCHES: usize = 3;
    assert!(
        ts_bytes.len() * LIVE_STARTUP_BATCHES < 2 * 1024 * 1024,
        "the persistent-pipe proof must emit before the old 2 MiB probe ceiling"
    );

    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let mut child = std::process::Command::new(ffmpeg)
        .args(build_stage_ffmpeg_args_for_input("h264", "h264", "hevc"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn bundled HEVC-to-H.264 transcode");
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 188 * 16];
        if let Ok(n) = std::io::Read::read(&mut stdout, &mut buf) {
            let _ = tx.send(n);
        }
    });
    let writer = std::thread::spawn(move || {
        for _ in 0..LIVE_STARTUP_BATCHES {
            for chunk in ts_bytes.chunks(1316) {
                if std::io::Write::write_all(&mut stdin, chunk).is_err() {
                    return stdin;
                }
            }
        }
        stdin
    });

    let live_bytes = match rx.recv_timeout(std::time::Duration::from_secs(12)) {
        Ok(n) => n,
        Err(err) => {
            let mut stdin = writer.join().expect("join writer");
            let _ = std::io::Write::flush(&mut stdin);
            drop(stdin);
            let _ = child.kill();
            let output = child.wait_with_output().expect("wait for ffmpeg");
            let _ = reader.join();
            panic!(
                "HEVC live pipe should emit stdout before stdin closes: {err}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    assert!(live_bytes > 0, "HEVC live pipe stdout should not be empty");

    let mut stdin = writer.join().expect("join writer");
    let _ = std::io::Write::flush(&mut stdin);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}
