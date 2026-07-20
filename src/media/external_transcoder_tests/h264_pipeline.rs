#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_h264_marker_fixture() {
    let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
        .expect("H.264 marker fixture");
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

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-h264-marker", "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-h264-marker",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-h264-marker", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new("pipe-ext-h264-marker", StageKind::video_preset("720p"));
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

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
            "external H.264 marker stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
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
        "external H.264 marker stage should emit live video packets"
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "external H.264 marker stage should emit a live keyframe"
    );
    assert!(
        source_ring.video_parameter_sets().is_some(),
        "source ring should cache raw parameter sets for the marker fixture"
    );
    assert!(
        reader.current_ring().video_parameter_sets().is_some(),
        "external H.264 marker stage output ring should cache raw parameter sets"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_h264_marker_fixture_without_eos() {
    let path = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h264",
        false,
        crate::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("H.264 marker fixture");
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

    let pipeline_id = "pipe-ext-h264-marker-live";
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest(pipeline_id, "stream-key", "srt")
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
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(pipeline_id, StageKind::video_preset("720p"));
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_live_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

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
            "external live H.264 marker stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let mut sent = 0usize;
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        let end = (sent + 8).min(packets.len());
        if sent < end {
            source_ring.push_batch(packets[sent..end].iter().cloned());
            sent = end;
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
        if sent == packets.len() {
            sent = 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    cancel.cancel();

    let snapshot = manager.snapshot(&stage_key).await;
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external H.264 marker stage should emit live video packets without requiring EOS; \
         source_write_idx={} output_write_idx={} output_packets={} stage_snapshot={snapshot:?}",
        source_ring.get_write_idx(),
        reader.current_ring().get_write_idx(),
        output_packets.len()
    );
}

#[tokio::test]
async fn external_1080p_stage_remuxes_marker_fixture_with_monotone_dts() {
    let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
        .expect("H.264 marker fixture");
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

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-h264-marker-1080p", "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-h264-marker-1080p",
            Some(video.clone()),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-h264-marker-1080p", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(audio_tracks.clone());
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(
        "pipe-ext-h264-marker-1080p",
        StageKind::video_preset("1080p"),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_1080p_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

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
            "external H.264 marker 1080p stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Video)
            .count()
            >= 120
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
        "external 1080p H.264 marker stage should emit live video packets"
    );

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();
    for packet in &output_packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }
    assert_strict_video_dts(
        "stage output",
        output_packets.iter().map(std::sync::Arc::as_ref),
    );

    let mut remux_demuxer = TsDemuxer::new();
    let mut remuxed_packets = Vec::new();
    for chunk in ts_bytes.chunks(1316) {
        remux_demuxer.feed(chunk);
        remux_demuxer.drain_into(&mut remuxed_packets);
    }
    remux_demuxer.flush();
    remux_demuxer.drain_into(&mut remuxed_packets);
    assert_strict_video_dts("remuxed output", remuxed_packets.iter());
}

