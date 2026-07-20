use super::*;

#[tokio::test]
async fn shared_ts_muxer_shares_across_multiple_readers() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;

    // Register active ingest so start_shared_ts_muxer can proceed
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();
    // Set metadata
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;

    // Create multiple stages or the same stage
    let stage1 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;
    let stage2 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;

    // Verify it is the exact same instance (same pointer)
    assert!(Arc::ptr_eq(&stage1, &stage2));

    // Create two readers
    let mut r1 = TsChunkReader::new("r1".to_string(), &stage1);
    let mut r2 = TsChunkReader::new("r2".to_string(), &stage1);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    // Push a video packet to the source ring
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
    });

    // Wait a bit for the tokio task to run and mux the packet
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let mut out1 = Vec::new();
    let mut out2 = Vec::new();
    assert_eq!(r1.pull_burst(&mut out1, 10).unwrap(), 1);
    assert_eq!(r2.pull_burst(&mut out2, 10).unwrap(), 1);

    assert_eq!(out1[0].payload, out2[0].payload);
    assert!(!out1[0].payload.is_empty());

    cancel_ingest.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_uses_routed_audio_track_metadata() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-routed-audio";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 0,
                    ..Default::default()
                },
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 1,
                    ..Default::default()
                },
            ],
        )
        .await;
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    }]);

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source+atrack:0", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("routed-audio-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    let (_, _, fixture_packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h264").expect("h264 fixture");
    let probe_ready_video = fixture_packets
        .iter()
        .find(|p| p.media_type == MediaType::Video && p.is_keyframe)
        .expect("fixture keyframe")
        .payload
        .clone();
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: probe_ready_video,
    });
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
    }
    demuxer.flush();
    let probe = demuxer.take_probe().expect("muxed TS should probe");
    assert_eq!(
        probe.audio_tracks.len(),
        1,
        "SRT subset muxer PMT must advertise only routed audio tracks"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_seeds_raw_hevc_parameter_sets_for_late_joiners() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-routed-hevc";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                track_index: 1,
                ..Default::default()
            }],
        )
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 1,
        ..Default::default()
    }]);
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    source_ring.set_video_parameter_sets(parameter_sets.clone());

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source+atrack:1", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("routed-hevc-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
    });
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 1,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&parameter_sets),
        "late-joining HEVC SRT muxer must prepend cached VPS/SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_prefers_preset_ring_parameter_sets_over_ingest_cache() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-preset-mismatch";

    // Raw ingest: registers an active ingest and caches an FLV AVC sequence
    // header, exactly as RTMP ingest does. This populates the pipeline-level
    // get_sequence_headers() cache with the *ingest's* (not the preset's)
    // SPS/PPS.
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    // FLV AVC sequence header tag body: [frame_type<<4|codec_id, pkt_type,
    // composition_time(3 bytes), AVCDecoderConfigurationRecord...].
    // AVCDecoderConfigurationRecord: version, profile, compat, level,
    // nalu_len_size byte, num_sps, sps_len(u16), sps..., num_pps,
    // pps_len(u16), pps... — encodes the ingest's own (1920x1080) SPS/PPS.
    let ingest_flv_seq_header = bytes::Bytes::from(vec![
        0x17, 0x00, 0x00, 0x00, 0x00, // FLV video tag header + composition time
        0x01, 0x64, 0x00, 0x1e, 0xFF, // AVCC version/profile/compat/level/nalu_len
        0x01, 0x00, 0x04, 0x67, 0x11, 0x22, 0x33, // 1 SPS, len 4
        0x01, 0x00, 0x04, 0x68, 0x44, 0x55, 0x66, // 1 PPS, len 4
    ]);
    let ingest_parameter_sets: &[u8] = &[
        0, 0, 0, 1, 0x67, 0x11, 0x22, 0x33, 0, 0, 0, 1, 0x68, 0x44, 0x55, 0x66,
    ];
    engine
        .cache_sequence_header(pipeline_id, true, ingest_flv_seq_header)
        .await;

    // Preset/transcoded ring: a distinct ring (e.g. the 720p transcoder's
    // output ring) with its own, different SPS/PPS.
    let preset_ring = Arc::new(RingBuffer::new(16));
    preset_ring.set_codec_hint("h264");
    let preset_parameter_sets = vec![
        0, 0, 0, 1, 0x67, 0xAA, 0xBB, 0xCC, 0, 0, 0, 1, 0x68, 0xDD, 0xEE, 0xFF,
    ];
    preset_ring.set_video_parameter_sets(preset_parameter_sets.clone());

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "720p", preset_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("preset-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&preset_ring).await;

    preset_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Flv,
        payload: bytes::Bytes::from(vec![
            0x17, 0x01, 0x00, 0x00, 0x00, // AVC keyframe packet, no composition offset
            0x00, 0x00, 0x00, 0x04, 0x65, 0xAB, 0xCD, 0xEF, // one 4-byte-length-prefixed NALU
        ]),
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(reader.pull_burst(&mut chunks, 10).unwrap() > 0);

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&preset_parameter_sets),
        "preset SRT muxer must prime from its own transcoded ring's parameter sets, \
             not the pipeline-level ingest sequence-header cache"
    );
    assert!(
        !video.payload.starts_with(ingest_parameter_sets),
        "preset SRT muxer must not seed the raw ingest's SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

#[tokio::test]
async fn shared_ts_muxer_replays_prebuffered_hevc_keyframe() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-prebuffered-hevc";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let cancel_ingest = engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(
            pipeline_id,
            vec![AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                track_index: 0,
                ..Default::default()
            }],
        )
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    }]);
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    source_ring.set_video_parameter_sets(parameter_sets.clone());
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 1000,
        dts: 1000,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
    });
    source_ring.push(crate::media::packet::MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 1020,
        dts: 1020,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x11; 32]),
    });

    let stage = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "source", source_ring.clone())
        .await;
    let mut reader = TsChunkReader::new("prebuffered-hevc-reader".to_string(), &stage);
    wait_for_shared_muxer_source_reader(&source_ring).await;

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut chunks = Vec::new();
    assert!(
        reader.pull_burst(&mut chunks, 10).unwrap() > 0,
        "late-joining shared muxer must replay the latest prebuffered HEVC keyframe"
    );

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in &chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let video = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("muxed TS should contain video");
    assert!(
        video.payload.starts_with(&parameter_sets),
        "prebuffered HEVC replay must include cached VPS/SPS/PPS"
    );

    cancel_ingest.cancel();
    stage.cancel.cancel();
}

async fn wait_for_shared_muxer_source_reader(source_ring: &Arc<RingBuffer>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.starts_with("ts_shared_muxer:"))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "shared muxer source reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shared_ts_muxer_cancels_and_recreates_after_probe_wait_exit() {
    let engine = Arc::new(crate::media::engine::MediaEngine::new());
    let pipeline_id = "test-pipe-probe-exit";
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;

    engine
        .try_register_ingest(pipeline_id, "key", "srt")
        .await
        .unwrap();

    let stage1 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring.clone())
        .await;

    engine.unregister_ingest(pipeline_id).await;

    tokio::time::timeout(std::time::Duration::from_secs(2), stage1.cancel.cancelled())
        .await
        .expect("shared muxer should cancel when ingest disappears before probe");
    assert!(stage1.cancel.is_cancelled());

    engine
        .try_register_ingest(pipeline_id, "key-2", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1280,
                height: 720,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;

    let stage2 = engine
        .get_or_create_ts_muxer_stage(pipeline_id, "play", source_ring)
        .await;

    assert!(
        !Arc::ptr_eq(&stage1, &stage2),
        "cancelled shared muxer stage must not be reused"
    );
    assert!(!stage2.cancel.is_cancelled());

    engine.unregister_ingest(pipeline_id).await;
    stage2.cancel.cancel();
}

#[tokio::test]
async fn benchmark_srt_sharing() {
    info!("\n=== SRT EGRESS SHARING BENCHMARK ===");
    let n_connections = 10;
    let n_packets = 2000;
    info!("Clients (N): {}, Packets (M): {}", n_connections, n_packets);

    let video_meta = VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio_track = crate::media::metadata::AudioMeta {
        track_index: 0,
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        profile: None,
        pid: None,
        language: None,
        title: None,
    };
    let audio_tracks = vec![audio_track];

    // Generate synthetic packets
    let mut packets = Vec::with_capacity(n_packets);
    let mut rng_seed = 0u8;
    for i in 0..n_packets {
        let is_video = i % 3 != 0;
        let is_keyframe = is_video && (i % 90 == 0);
        let media_type = if is_video {
            MediaType::Video
        } else {
            MediaType::Audio
        };
        let size = if is_video {
            if is_keyframe { 100_000 } else { 10_000 }
        } else {
            500
        };
        rng_seed = rng_seed.wrapping_add(1);
        let payload = bytes::Bytes::from(vec![rng_seed; size]);
        packets.push(crate::media::packet::MediaPacket {
            media_type,
            track_index: 0,
            pts: i as i64 * 33,
            dts: i as i64 * 33,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload,
        });
    }

    // --- OLD ARCHITECTURE: Independent Muxing ---
    let start_old = Instant::now();
    let mut old_handles = Vec::new();
    for _ in 0..n_connections {
        let packets_clone = packets.clone();
        let video_meta_clone = video_meta.clone();
        let audio_tracks_clone = audio_tracks.clone();
        let handle = tokio::spawn(async move {
            let mut muxer =
                crate::media::mpegts::TsMuxer::new(Some(&video_meta_clone), &audio_tracks_clone);
            let mut bytes_written = 0u64;
            for pkt in &packets_clone {
                let ts_bytes = muxer.mux_packet(
                    pkt.media_type,
                    pkt.track_index,
                    pkt.pts,
                    pkt.dts,
                    pkt.is_keyframe,
                    &pkt.payload,
                );
                bytes_written += ts_bytes.len() as u64;
            }
            bytes_written
        });
        old_handles.push(handle);
    }

    let mut total_bytes_old = 0u64;
    for h in old_handles {
        total_bytes_old += h.await.unwrap();
    }
    let elapsed_old = start_old.elapsed();

    // --- NEW ARCHITECTURE: Shared Muxing ---
    let start_new = Instant::now();
    let ts_ring = Arc::new(TsChunkRing::new(4096, CancellationToken::new()));
    let mut readers = Vec::new();
    for i in 0..n_connections {
        readers.push(TsChunkReader::new(format!("reader_{}", i), &ts_ring));
    }

    let mut new_handles = Vec::new();
    for mut reader in readers {
        let handle = tokio::spawn(async move {
            let mut chunks_received = 0;
            let mut bytes_received = 0u64;
            let mut out_burst = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
            while chunks_received < n_packets {
                out_burst.clear();
                match reader.pull_burst(&mut out_burst, MEDIA_PULL_BURST_PACKETS) {
                    Ok(0) => {
                        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                    }
                    Ok(count) => {
                        chunks_received += count;
                        for chunk in &out_burst {
                            bytes_received += chunk.payload.len() as u64;
                        }
                    }
                    Err(_) => {}
                }
            }
            bytes_received
        });
        new_handles.push(handle);
    }

    // Shared muxer task
    let ts_ring_clone = ts_ring.clone();
    let packets_clone = packets.clone();
    let video_meta_clone = video_meta.clone();
    let audio_tracks_clone = audio_tracks.clone();
    let muxer_handle = tokio::spawn(async move {
        let mut muxer =
            crate::media::mpegts::TsMuxer::new(Some(&video_meta_clone), &audio_tracks_clone);
        for pkt in &packets_clone {
            let ts_bytes = muxer.mux_packet(
                pkt.media_type,
                pkt.track_index,
                pkt.pts,
                pkt.dts,
                pkt.is_keyframe,
                &pkt.payload,
            );
            ts_ring_clone.push(bytes::Bytes::copy_from_slice(ts_bytes), pkt.is_keyframe);
        }
    });

    muxer_handle.await.unwrap();

    let mut total_bytes_new = 0u64;
    for h in new_handles {
        total_bytes_new += h.await.unwrap();
    }
    let elapsed_new = start_new.elapsed();

    info!("Old Architecture Time: {:?}", elapsed_old);
    info!("New Architecture Time: {:?}", elapsed_new);
    info!("Old Total Bytes Muxed: {}", total_bytes_old);
    info!("New Total Bytes Muxed: {}", total_bytes_new);

    assert_eq!(total_bytes_old, total_bytes_new);

    let ratio = elapsed_old.as_secs_f64() / elapsed_new.as_secs_f64();
    info!("Performance Gain Ratio: {:.2}x", ratio);
    info!("=====================================");
}
