//! Helpers shared by media tests and their dependents' tests: compiled for
//! this crate's tests and, with the `test-support` feature, for dev builds
//! of the crates above it. Shipped builds leave the feature off.

use std::time::Duration;
use std::{cell::Cell, panic, sync::Once};

use bytes::Bytes;
use restream_core::{MediaPacket, MediaType, PayloadFormat};

use crate::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::policy::LeafPolicy;
use crate::egress::shard::EgressShardConfig;

thread_local! {
    static SUPPRESS_EXPECTED_PANIC_HOOK: Cell<bool> = const { Cell::new(false) };
}

static INSTALL_PANIC_HOOK: Once = Once::new();

struct RestoreSuppression(bool);

impl Drop for RestoreSuppression {
    fn drop(&mut self) {
        SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.set(self.0));
    }
}

pub fn with_expected_panic_suppressed<R>(operation: impl FnOnce() -> R) -> R {
    INSTALL_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !SUPPRESS_EXPECTED_PANIC_HOOK.with(Cell::get) {
                previous(info);
            }
        }));
    });

    let previous = SUPPRESS_EXPECTED_PANIC_HOOK.with(|suppressed| suppressed.replace(true));
    let _restore = RestoreSuppression(previous);
    operation()
}

pub fn test_video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"video"),
    }
}

pub fn test_audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"audio"),
    }
}

pub fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
}

pub fn output_spec(id: &str, feed: &FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: feed.clone(),
        protocol: ProtocolSpec::Sink,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}
