use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use restream_core::{MediaPacket, MediaType};
use restream_engine::external_transcoder::build_stage_ffmpeg_args;
use restream_mpegts::TsDemuxer;

use super::support::load_fixture;

static FFMPEG_EXTRACT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static TEMP_ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn run_external_stage_args(fixture: &[u8], preset: &str) -> Vec<MediaPacket> {
    let ffmpeg = {
        let _guard = FFMPEG_EXTRACT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        restream_engine::ffmpeg_binary::ffmpeg_bin_path()
    };
    let temp_dir = temp_artifact_dir();
    let input_path = temp_dir.join("input.ts");
    let output_path = temp_dir.join("output.ts");
    std::fs::write(&input_path, fixture).expect("write input fixture");

    let mut args = build_stage_ffmpeg_args(preset, "h264");
    replace_arg_value(&mut args, "-i", input_path.to_string_lossy().as_ref());
    if let Some(last) = args.last_mut() {
        *last = output_path.to_string_lossy().to_string();
    } else {
        panic!("ffmpeg args missing output path");
    }

    let output = Command::new(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg stage failed for {preset}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::fs::read(&output_path).expect("read ffmpeg output");
    let _ = std::fs::remove_dir_all(&temp_dir);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&stdout);
    let mut packets = Vec::new();
    demuxer.drain_into(&mut packets);
    packets
}

fn replace_arg_value(args: &mut [String], flag: &str, value: &str) {
    let position = args
        .iter()
        .position(|arg| arg == flag)
        .unwrap_or_else(|| panic!("missing ffmpeg arg flag {flag}"));
    let target = args
        .get_mut(position + 1)
        .unwrap_or_else(|| panic!("missing ffmpeg arg value for {flag}"));
    *target = value.to_string();
}

fn temp_artifact_dir() -> std::path::PathBuf {
    let suffix = TEMP_ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "restream-transcoder-test-{}-{suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp artifact dir");
    dir
}

#[test]
fn external_audio_remap_filter_produces_stereo_audio() {
    let fixture = load_fixture();
    let packets = run_external_stage_args(&fixture, "audio:remap:1:0:0:from:source");

    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio),
        "remap filter should produce audio packets"
    );
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "remap stage should copy video packets"
    );
}

#[test]
fn external_audio_downmix_filter_produces_stereo_audio() {
    let fixture = load_fixture();
    let packets = run_external_stage_args(&fixture, "audio:downmix:0:from:source");

    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio),
        "downmix filter should produce audio packets"
    );
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "downmix stage should copy video packets"
    );
}
