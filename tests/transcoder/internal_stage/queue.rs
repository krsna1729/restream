use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proptest::prelude::{ProptestConfig, prop};
use proptest::test_runner::FileFailurePersistence;
use proptest::{prop_assert, proptest};
use restream::media::avio::MemoryQueue;
use restream::media::packet::{MediaPacket, MediaType};
use restream::media::ring_buffer::{Reader, RingBuffer};
use restream::media::transcoder::{run_ffmpeg_transcode_with_scale, run_ffmpeg_transcoder_stage};
use tokio_util::sync::CancellationToken;

use super::fixture::synthetic_video_only_ts_limited;
use crate::support::load_fixture;

fn feed_queue_with_chunk_pattern(
    input: &Arc<MemoryQueue>,
    payload: &[u8],
    chunk_pattern: &[usize],
) {
    assert!(!chunk_pattern.is_empty(), "chunk pattern must not be empty");

    let mut offset = 0usize;
    let mut pattern_index = 0usize;
    while offset < payload.len() {
        let chunk_len = chunk_pattern[pattern_index % chunk_pattern.len()].max(1);
        let end = (offset + chunk_len).min(payload.len());
        input.write_sync(&payload[offset..end]);
        offset = end;
        pattern_index += 1;
    }
}

fn assert_packets_have_monotonic_dts_per_stream(packets: &[Arc<MediaPacket>]) {
    let mut last_dts_by_stream = HashMap::<(bool, u32), i64>::new();
    for (index, packet) in packets.iter().enumerate() {
        let stream_key = (packet.media_type == MediaType::Video, packet.track_index);
        if let Some(previous_dts) = last_dts_by_stream.get(&stream_key) {
            assert!(
                packet.dts >= *previous_dts,
                "dts regression at output packet {index} for {:?}/track {}: {} -> {}",
                packet.media_type,
                packet.track_index,
                previous_dts,
                packet.dts
            );
        }
        last_dts_by_stream.insert(stream_key, packet.dts);
    }
}

#[test]
fn internal_scale_stage_chunked_remux_input_preserves_video_timestamp_order() {
    let fixture = load_fixture();
    let synthetic_ts = synthetic_video_only_ts_limited(&fixture, 180);

    let input = Arc::new(MemoryQueue::new());
    let output = Arc::new(RingBuffer::new(4096));

    // Split writes across irregular boundaries to prove the in-process
    // demux/decode path is insensitive to queue chunking.
    feed_queue_with_chunk_pattern(&input, &synthetic_ts, &[7, 188, 31, 512, 93, 2048]);
    input.close();

    let result =
        run_ffmpeg_transcode_with_scale(input, output.clone(), "720p", CancellationToken::new());
    assert!(
        result.is_ok(),
        "run_ffmpeg_transcode_with_scale failed for chunked remux input: {result:?}"
    );

    let mut reader = Reader::new("internal_scale_chunked_remux".to_string(), output);
    let mut packets = Vec::new();
    while let Ok(Some(packet)) = reader.pull() {
        packets.push(packet);
    }

    assert!(
        !packets.is_empty(),
        "internal scale stage should emit packets"
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video),
        "video-only remux input should emit only video packets"
    );
    assert_packets_have_monotonic_dts_per_stream(&packets);
}

#[test]
fn internal_scale_stage_emits_before_live_queue_closes() {
    // Regression: file-ingest feeds a complete queue and hid the fact that an
    // in-process AVIO reader can wait indefinitely on a persistent SRT queue.
    // This committed transport fixture needs a 768 KiB keyframe lead-in; keep
    // the queue open after that valid startup window and require video before
    // EOF, matching the live stage contract.
    const LIVE_STARTUP_BYTES: usize = 768 * 1024;
    let fixture = load_fixture();
    assert!(
        fixture.len() > LIVE_STARTUP_BYTES,
        "checked-in H.264 fixture must exercise a bounded live startup window"
    );

    let input = Arc::new(MemoryQueue::new());
    let output = Arc::new(RingBuffer::new(4096));
    let cancel = CancellationToken::new();
    let worker_input = input.clone();
    let worker_output = output.clone();
    let worker_cancel = cancel.clone();
    let worker = std::thread::spawn(move || {
        run_ffmpeg_transcode_with_scale(worker_input, worker_output, "720p", worker_cancel)
    });

    input.write_sync(&fixture[..LIVE_STARTUP_BYTES]);
    let mut reader = Reader::new("internal_live_queue_startup".to_string(), output);
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut emitted_video = false;
    while Instant::now() < deadline {
        while let Ok(Some(packet)) = reader.pull() {
            if packet.media_type == MediaType::Video {
                emitted_video = true;
                break;
            }
        }
        if emitted_video {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    cancel.cancel();
    input.close();
    let result = worker
        .join()
        .expect("internal scale worker should not panic");
    assert!(result.is_ok(), "internal scale worker failed: {result:?}");
    assert!(
        emitted_video,
        "internal scale stage must emit before a live queue reaches EOF"
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 6,
        max_shrink_iters: 0,
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions"
        ))),
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_source_stage_chunked_input_preserves_per_stream_dts_order(
        chunk_pattern in prop::collection::vec(1usize..2048, 1..16)
    ) {
        let fixture = load_fixture();
        let input = Arc::new(MemoryQueue::new());
        let output = Arc::new(RingBuffer::new(4096));

        feed_queue_with_chunk_pattern(&input, &fixture, &chunk_pattern);
        input.close();

        let result = run_ffmpeg_transcoder_stage(
            input,
            output.clone(),
            "source",
            CancellationToken::new(),
        );
        prop_assert!(
            result.is_ok(),
            "source stage failed for chunk pattern {:?}: {:?}",
            chunk_pattern,
            result
        );

        let mut reader = Reader::new("prop_source_chunked_dts".to_string(), output);
        let mut packets = Vec::new();
        while let Ok(Some(packet)) = reader.pull() {
            packets.push(packet);
        }

        prop_assert!(!packets.is_empty(), "source stage produced no packets");
        assert_packets_have_monotonic_dts_per_stream(&packets);
        for packet in &packets {
            prop_assert!(packet.pts >= 0, "pts must remain non-negative");
            prop_assert!(packet.dts >= 0, "dts must remain non-negative");
        }
    }
}
