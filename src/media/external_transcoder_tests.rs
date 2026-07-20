use super::*;
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::AudioMeta;
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::MediaType;
use crate::media::ring_buffer::{DtsEnforcer, Reader, RingBuffer};
use crate::media::stage_runtime::{build_ffmpeg_stage_plan, wait_for_stage_metadata};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use tokio_util::sync::CancellationToken;

// Helpers shared by the argument, metadata, and live-stage behavior slices.
fn write_temp_ts_artifact(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "restream-external-transcoder-test-{}-{}",
        std::process::id(),
        name
    ));
    std::fs::create_dir_all(&dir).expect("create temp artifact dir");
    let path = dir.join("artifact.ts");
    std::fs::write(&path, bytes).expect("write temp TS artifact");
    path
}

fn assert_strict_video_dts<'a, I>(label: &str, packets: I)
where
    I: IntoIterator<Item = &'a crate::media::packet::MediaPacket>,
{
    let mut previous = None;
    let mut count = 0usize;
    for packet in packets
        .into_iter()
        .filter(|packet| packet.media_type == MediaType::Video)
    {
        if let Some(previous_dts) = previous {
            assert!(
                packet.dts > previous_dts,
                "{label} video DTS must be strictly increasing: {previous_dts} >= {}",
                packet.dts
            );
        }
        previous = Some(packet.dts);
        count += 1;
    }
    assert!(count > 0, "{label} should include video packets");
}

fn test_audio_track(track_index: u32) -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: Some("stereo".to_string()),
        track_index,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }
}

include!("external_transcoder_tests/arguments.rs");
include!("external_transcoder_tests/metadata.rs");
include!("external_transcoder_tests/hevc_pipeline.rs");
include!("external_transcoder_tests/h264_pipeline.rs");
include!("external_transcoder_tests/hevc_fixtures.rs");
