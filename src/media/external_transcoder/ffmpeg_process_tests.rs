use super::*;
use crate::domain::transcode_profile::TranscodeProfile;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::mpegts::TsDemuxer;
use proptest::prelude::*;
use std::sync::{Mutex, MutexGuard};

static PROFILE_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn profile_cache_test_lock() -> MutexGuard<'static, ()> {
    PROFILE_CACHE_TEST_LOCK
        .lock()
        .expect("profile cache test lock")
}

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

fn arg_after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let pos = args
        .iter()
        .position(|arg| arg == flag)
        .unwrap_or_else(|| panic!("missing ffmpeg arg {flag}"));
    &args[pos + 1]
}

fn preset_for_probe_property(case: u8, audio_track_count: usize) -> String {
    match case % 6 {
        0 => "720p".to_string(),
        1 => "1080p".to_string(),
        2 => "source".to_string(),
        3 => format!("downmix:{}", audio_track_count.saturating_sub(1)),
        4 => format!("remap:0:1:{}", audio_track_count.saturating_sub(1)),
        _ => "audio:atrack:0:from:720p".to_string(),
    }
}

include!("ffmpeg_process_tests/arguments.rs");
include!("ffmpeg_process_tests/transcoding.rs");
