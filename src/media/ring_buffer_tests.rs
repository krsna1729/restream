use super::*;
use bytes::Bytes;
use std::sync::Mutex;

static EXPECTED_PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct ScopedSilentPanicHook(Option<PanicHook>);

impl ScopedSilentPanicHook {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn silence(&mut self) {
        std::panic::set_hook(Box::new(|_| {}));
    }
}

impl Drop for ScopedSilentPanicHook {
    fn drop(&mut self) {
        if let Some(hook) = self.0.take() {
            std::panic::set_hook(hook);
        }
    }
}

fn video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts,
        is_keyframe: keyframe,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 16]),
    }
}

fn audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts,
        dts,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 4]),
    }
}

fn media_packet_with_payload(media_type: MediaType, dts: i64, payload_bytes: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        track_index: 0,
        pts: dts,
        dts,
        is_keyframe: matches!(media_type, MediaType::Video) && dts == 0,
        format: PayloadFormat::Raw,
        payload: Bytes::from(vec![0; payload_bytes]),
    }
}

#[path = "ring_buffer_tests/concurrency.rs"]
mod concurrency;
#[path = "ring_buffer_tests/overflow.rs"]
mod overflow;
#[path = "ring_buffer_tests/reader.rs"]
mod reader;
