use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use restream::media::avio::MemoryQueue;
use restream::media::external_transcoder::build_stage_ffmpeg_args;
use restream::media::mpegts::TsDemuxer;
use restream::media::packet::{MediaPacket, MediaType};
use restream::media::ring_buffer::{Reader, RingBuffer};
use restream::media::transcoder::run_ffmpeg_transcoder_stage;
use tokio_util::sync::CancellationToken;

use super::support::load_fixture;

static FFMPEG_EXTRACT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static TEMP_ARTIFACT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn run_stage(fixture: &[u8], preset: &str) -> (Vec<Arc<MediaPacket>>, bool) {
    let input = Arc::new(MemoryQueue::new());
    let output = Arc::new(RingBuffer::new(4096));
    {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(input.write(fixture));
    }
    input.close();

    let result =
        run_ffmpeg_transcoder_stage(input, output.clone(), preset, CancellationToken::new());

    let mut reader = Reader::new("test_transcoder".to_string(), output);
    let mut packets = Vec::new();
    while let Ok(Some(packet)) = reader.pull() {
        packets.push(packet);
    }

    (packets, result.is_ok())
}

fn run_external_stage_args(fixture: &[u8], preset: &str) -> Vec<MediaPacket> {
    let ffmpeg = {
        let _guard = FFMPEG_EXTRACT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        restream::ffmpeg_extract::ensure_ffmpeg_extracted()
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
fn source_passthrough_produces_output() {
    let fixture = load_fixture();
    let (packets, ok) = run_stage(&fixture, "source");
    assert!(ok, "transcoder stage failed for preset 'source'");
    assert!(
        !packets.is_empty(),
        "no packets produced for preset 'source'"
    );

    let video_count = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Audio)
        .count();
    assert!(video_count > 0, "no video packets in output");
    assert!(audio_count > 0, "no audio packets in output");
}

#[test]
fn video_720p_preset_produces_output() {
    let fixture = load_fixture();
    let (packets, ok) = run_stage(&fixture, "video:720p");
    assert!(ok, "transcoder stage failed for preset 'video:720p'");
    assert!(
        !packets.is_empty(),
        "no packets produced for preset 'video:720p'"
    );

    let video_count = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video)
        .count();
    assert!(video_count > 0, "no video packets in 720p output");
}

#[test]
fn audio_routing_atrack_filters_correctly() {
    let fixture = load_fixture();
    // Single audio track in fixture, selecting track 0 should pass it through
    let (packets, ok) = run_stage(&fixture, "source+atrack:0");
    assert!(ok, "transcoder stage failed for preset 'source+atrack:0'");

    let audio_count = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Audio)
        .count();
    assert!(audio_count > 0, "atrack:0 should include the audio track");

    // Selecting a non-existent track should produce no audio
    let (packets2, ok2) = run_stage(&fixture, "source+atrack:5");
    assert!(ok2, "transcoder stage failed for preset 'source+atrack:5'");

    let audio_count2 = packets2
        .iter()
        .filter(|packet| packet.media_type == MediaType::Audio)
        .count();
    assert_eq!(
        audio_count2, 0,
        "atrack:5 should exclude all audio (only 1 track in fixture)"
    );
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

#[test]
fn cancelled_token_stops_early() {
    let fixture = load_fixture();
    let input = Arc::new(MemoryQueue::new());
    let output = Arc::new(RingBuffer::new(4096));
    {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(input.write(&fixture));
    }
    input.close();

    let token = CancellationToken::new();
    token.cancel();

    let result = run_ffmpeg_transcoder_stage(input, output.clone(), "source", token);
    assert!(result.is_ok(), "cancelled transcoder should exit cleanly");
}
