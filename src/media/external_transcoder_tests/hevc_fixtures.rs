#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-single-audio", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-single-audio",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-single-audio", audio_tracks.clone())
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
        "pipe-ext-hevc-single-audio",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_single_audio_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

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
            "external 720p single-audio HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
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
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p single-audio HEVC stage should emit live video packets"
    );
    assert!(
        reader.current_ring().video_parameter_sets().is_some(),
        "external 720p HEVC stage output ring should cache raw parameter sets for chained stages"
    );
}

#[tokio::test]
async fn external_h264_stage_emits_live_packets_for_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-source-h264", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-source-h264",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-source-h264", audio_tracks.clone())
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
        "pipe-ext-hevc-source-h264",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_single_audio_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

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
            "external source-h264 single-audio HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
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
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external source-h264 single-audio HEVC stage should emit live video packets"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_video_for_prebuffered_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");
    let continuation = packets
        .iter()
        .rev()
        .take(96)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-prebuffered", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-prebuffered",
            Some(video.clone()),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-prebuffered", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks.clone());
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    source_ring.push_batch(packets.drain(..));

    let stage_key = StageKey::new(
        "pipe-ext-hevc-prebuffered",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_prebuffered_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    source_ring.push_batch(continuation);

    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
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
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC stage should emit video once a prebuffered join receives live continuation"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_canonical_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview-long", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview-long",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview-long", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(32_768));
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
        "pipe-ext-preview-long",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_long_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

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
            "external 720p long-input HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));

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
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC preview stage should emit live video packets for the canonical fixture (got {} packets)",
        output_packets.len()
    );
}

#[test]
fn feeder_remuxed_single_audio_hevc_fixture_decodes_as_ts_file() {
    let (video, audio_tracks, packets) = crate::test_fixtures::primary_av_packets_for_codec("h265")
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

    assert!(
        !ts_bytes.is_empty(),
        "remuxed HEVC fixture should produce TS bytes"
    );

    let ts_path = write_temp_ts_artifact("hevc-feeder-remux", &ts_bytes);
    let output = std::process::Command::new(crate::ffmpeg_extract::ensure_ffmpeg_extracted())
        .args([
            "-nostdin",
            "-v",
            "error",
            "-i",
            ts_path.to_str().expect("utf-8 ts path"),
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg decode check");

    assert!(
        output.status.success(),
        "ffmpeg should decode feeder-remuxed HEVC TS: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
