use restream::media::packet::{MediaType, PayloadFormat};

use super::fixture::{run_internal_scale_stage, synthetic_video_only_ts};
use crate::support::load_fixture;

#[test]
fn internal_transcode_builtin_video_presets_produce_video() {
    let fixture = load_fixture();
    let synthetic_ts = synthetic_video_only_ts(&fixture);

    for preset in ["h264", "720p", "1080p"] {
        let output_packets = run_internal_scale_stage(&synthetic_ts, preset);

        assert!(
            !output_packets.is_empty(),
            "no packets produced by internal transcode preset {preset}"
        );
        assert!(
            output_packets.iter().any(|packet| packet.is_keyframe),
            "internal transcode preset {preset} should emit a keyframe"
        );
        for packet in &output_packets {
            assert_eq!(
                packet.media_type,
                MediaType::Video,
                "expected only video packets for preset {preset}"
            );
            assert_eq!(
                packet.format,
                PayloadFormat::Raw,
                "expected raw encoded packets for preset {preset}"
            );
        }
    }
}
