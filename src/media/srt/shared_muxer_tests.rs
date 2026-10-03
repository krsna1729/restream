use super::*;

fn test_packet(media_type: MediaType, payload_len: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        format: crate::media::packet::PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from(vec![0u8; payload_len]),
    }
}

#[test]
fn estimate_ts_accum_capacity_floors_at_188_for_empty_burst() {
    assert_eq!(estimate_ts_accum_capacity(&[]), 188);
}

#[test]
fn estimate_ts_accum_capacity_floors_at_188_for_tiny_payloads() {
    // A single zero-length packet still needs at least one TS packet's
    // worth of muxer overhead, not a 0-capacity allocation.
    let packets = vec![Arc::new(test_packet(MediaType::Video, 0))];
    assert_eq!(estimate_ts_accum_capacity(&packets), 188 * 4);
}

#[test]
fn estimate_ts_accum_capacity_sums_payload_plus_ts_packet_overhead() {
    let packets = vec![
        Arc::new(test_packet(MediaType::Video, 100)),
        Arc::new(test_packet(MediaType::Audio, 50)),
    ];
    assert_eq!(
        estimate_ts_accum_capacity(&packets),
        (100 + 188 * 4) + (50 + 188 * 4)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_muxer_packages_media_while_control_thread_is_blocked() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "mux-control-isolation";
    let (video, audio, packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h264").unwrap();
    engine
        .try_register_ingest(pipeline_id, "mux-isolation", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(pipeline_id, Some(video), audio.first().cloned(), None)
        .await;
    let source = Arc::new(RingBuffer::new(4096));
    source.set_audio_tracks(audio);
    source.push_batch(packets.iter().take(64).cloned());
    let cancel = CancellationToken::new();
    let ts_ring = start_shared_ts_muxer(
        pipeline_id,
        "source",
        source,
        engine.clone(),
        cancel.clone(),
    );
    let mut reader =
        crate::media::ts_chunk_ring::TsChunkReader::new("mux-isolation-result".into(), &ts_ring);
    let (done, observed) = std::sync::mpsc::channel();
    let observer = crate::media::executor::spawn(cancel.clone(), async move {
        let mut chunks = Vec::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            reader.wait_for_data_or_cancelled().await;
            reader.pull_burst(&mut chunks, 4096).unwrap();
            chunks
        })
        .await;
        let _ = done.send(result);
    })
    .unwrap();
    // No control-runtime polls are possible until the muxer has published TS.
    let chunks = observed
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    cancel.cancel();
    observer.await.unwrap();
    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut remuxed = Vec::new();
    for chunk in chunks {
        demuxer.feed(&chunk.payload);
        demuxer.drain_into(&mut remuxed);
    }
    demuxer.flush();
    demuxer.drain_into(&mut remuxed);
    let first_video = remuxed
        .iter()
        .find(|p| p.media_type == MediaType::Video)
        .unwrap();
    let expected_video = packets
        .iter()
        .find(|p| p.media_type == MediaType::Video)
        .unwrap();
    assert_eq!(first_video.payload, expected_video.payload);
    assert_eq!(
        (first_video.pts, first_video.dts),
        (expected_video.pts, expected_video.dts)
    );
}
