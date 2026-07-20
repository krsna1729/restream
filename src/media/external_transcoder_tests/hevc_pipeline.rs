#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_hevc_sample() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let _ = tracing_subscriber::fmt::try_init();
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    // Extract parameter sets from the pre-demuxed packets so the stage's
    // metadata wait loop can find them (required for HEVC).
    if let Some(ps) = packets.iter().find_map(|p| {
        (p.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&p.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(ps);
    }
    let stage_key = StageKey::new(
        "pipe-ext-preview",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external 720p stage reader did not attach to the source ring in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Feed all input.  With 18 streams (2v + 16a) FFmpeg holds output
    // until stdin closes, so mark EOS and let the pump drain naturally.
    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|p| p.media_type == MediaType::Video && p.is_keyframe)
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC preview stage should emit video packets after close (got {} packets)",
        output_packets.len()
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "external 720p HEVC preview stage should emit a keyframe after close (got {} packets)",
        output_packets.len()
    );
    cancel.cancel();
}

#[tokio::test]
async fn chained_hevc_preview_stages_emit_live_h264_packets() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview-chain", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview-chain",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview-chain", audio_tracks.clone())
        .await;

    let source_ring = engine
        .get_or_create_pipeline("pipe-ext-preview-chain")
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let hevc_stage_key = StageKey::new("pipe-ext-preview-chain", StageKind::video_preset("1080p"));
    let hevc_preview_upstream = engine
        .get_or_create_transcoder(
            "pipe-ext-preview-chain",
            StageKind::video_preset("1080p"),
            source_ring.clone(),
            Some("hevc"),
        )
        .await;
    let h264_stage_key = StageKey::new(
        "pipe-ext-preview-chain",
        StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("1080p")),
    );
    let h264_preview_ring = engine
        .get_or_create_h264_transcoder(
            "pipe-ext-preview-chain",
            StageKind::video_preset("1080p"),
            hevc_preview_upstream.clone(),
        )
        .await;
    let mut hevc_reader = Reader::new_live(
        "test_ext_preview_chain_mid".to_string(),
        hevc_preview_upstream.clone(),
    );
    let mut reader = Reader::new_live(
        "test_ext_preview_chain_output".to_string(),
        h264_preview_ring.clone(),
    );

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let source_attached = source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&hevc_stage_key.to_string()));
        if source_attached {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "preview upstream reader did not attach in time: source={:?} expected_source={} hevc_stage={:?}",
            source_ring.reader_snapshots(),
            hevc_stage_key,
            engine.stage_runtime_snapshot(&hevc_stage_key).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    let mut hevc_packets = Vec::new();
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = hevc_reader.pull() {
            hevc_packets.push(packet);
        }
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    assert_eq!(
        h264_preview_ring.codec_hint_str(),
        "h264",
        "preview codec-edge ring should advertise H.264 output"
    );
    assert!(
        hevc_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "preview chain should first emit live HEVC packets from the 1080p stage"
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "chained HEVC preview stages should emit live video packets; h264_stage={:?}",
        engine.stage_runtime_snapshot(&h264_stage_key).await
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "chained HEVC preview stages should emit a live keyframe"
    );
}

#[tokio::test]
async fn hevc_scaled_rtmp_audio_routes_emit_both_selected_tracks() {
    let fixture = crate::test_fixtures::av_marker_transport_fixture("h265", true)
        .expect("multi-audio HEVC marker fixture");
    let fixture_bytes = std::fs::read(fixture).expect("read multi-audio HEVC marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in fixture_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer
        .take_probe()
        .expect("probe multi-audio HEVC marker fixture");
    let video = probe.video.expect("sample should contain video");
    let audio_tracks = probe.audio_tracks;
    assert!(
        audio_tracks.len() >= 2,
        "fixture must expose at least two audio tracks"
    );

    let pipeline_id = "pipe-ext-scaled-audio-routes";
    let engine = Arc::new(MediaEngine::new());
    let _ = tracing_subscriber::fmt::try_init();
    engine
        .try_register_ingest(pipeline_id, "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(pipeline_id, audio_tracks.clone())
        .await;

    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let scaled_hevc = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::video_preset("720p"),
            source_ring.clone(),
            Some("hevc"),
        )
        .await;
    let scaled_h264 = engine
        .get_or_create_h264_transcoder(
            pipeline_id,
            StageKind::video_preset("720p"),
            scaled_hevc.clone(),
        )
        .await;
    let track0_ring = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::audio_route(
                "atrack:0",
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
            ),
            scaled_h264.clone(),
            None,
        )
        .await;
    let track1_ring = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::audio_route(
                "atrack:1",
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
            ),
            scaled_h264.clone(),
            None,
        )
        .await;

    let mut scaled_hevc_reader =
        Reader::new_live("test_ext_scaled_hevc_mid".to_string(), scaled_hevc.clone());
    let mut scaled_h264_reader =
        Reader::new_live("test_ext_scaled_h264_mid".to_string(), scaled_h264.clone());
    let mut track0_reader =
        Reader::new_live("test_ext_scaled_audio_track0".to_string(), track0_ring);
    let mut track1_reader =
        Reader::new_live("test_ext_scaled_audio_track1".to_string(), track1_ring);

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let source_attached = source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains("video:720p"));
        let h264_attached = scaled_h264.reader_snapshots().len() >= 2;
        if source_attached && h264_attached {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "scaled RTMP selected-audio chain readers did not attach in time: \
             source={:?} scaled_hevc={:?} scaled_h264={:?}",
            source_ring.reader_snapshots(),
            scaled_hevc.reader_snapshots(),
            scaled_h264.reader_snapshots()
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut scaled_hevc_packets = Vec::new();
    let mut scaled_h264_packets = Vec::new();
    let mut track0_packets = Vec::new();
    let mut track1_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = scaled_hevc_reader.pull() {
            scaled_hevc_packets.push(packet);
        }
        while let Ok(Some(packet)) = scaled_h264_reader.pull() {
            scaled_h264_packets.push(packet);
        }
        while let Ok(Some(packet)) = track0_reader.pull() {
            track0_packets.push(packet);
        }
        while let Ok(Some(packet)) = track1_reader.pull() {
            track1_packets.push(packet);
        }
        let track0_ready = selected_track_output_ready(&track0_packets);
        let track1_ready = selected_track_output_ready(&track1_packets);
        if track0_ready && track1_ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < output_deadline,
            "scaled RTMP selected-audio routes did not both emit video and selected audio: \
             source_write={} source_eos={} scaled_hevc_write={} scaled_hevc_eos={} \
             scaled_h264_write={} scaled_h264_eos={} scaled_hevc_packets={} \
             scaled_h264_packets={} track0_packets={} track1_packets={} stages={:?}",
            source_ring.get_write_idx(),
            source_ring.is_end_of_stream(),
            scaled_hevc.get_write_idx(),
            scaled_hevc.is_end_of_stream(),
            scaled_h264.get_write_idx(),
            scaled_h264.is_end_of_stream(),
            scaled_hevc_packets.len(),
            scaled_h264_packets.len(),
            track0_packets.len(),
            track1_packets.len(),
            engine.pipeline_stage_runtime_snapshots(pipeline_id).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn selected_track_output_ready(
    packets: &[std::sync::Arc<crate::media::packet::MediaPacket>],
) -> bool {
    packets
        .iter()
        .any(|packet| packet.media_type == MediaType::Video)
        && packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio && packet.track_index == 0)
        && packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .all(|packet| packet.track_index == 0)
}

#[tokio::test]
async fn hevc_hls_preview_stage_uses_hevc_input_and_emits_h264() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
    engine
        .try_register_ingest("pipe-hevc-preview-input", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-hevc-preview-input",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-hevc-preview-input", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let stage_key = StageKey::new(
        "pipe-hevc-preview-input",
        StageKind::preview("720p", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    assert_eq!(
        output_ring.codec_hint_str(),
        "h264",
        "preview output ring should advertise browser-safe H.264"
    );
    let mut reader = Reader::new_live("test_hevc_preview_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_preview_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "HEVC preview stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();

    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "HEVC preview stage should demux HEVC input and emit H.264 video"
    );
    assert!(
        output_packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video),
        "preview stage should drop audio packets"
    );
}
