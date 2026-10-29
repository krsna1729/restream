//! Application facade for the packet vocabulary implemented in `restream-core`.
//!
//! Producers, transforms and consumers share these transport-independent types;
//! this module re-exports them rather than defining a second representation.

pub use restream_core::{MediaPacket, MediaType, PayloadFormat};
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn media_packet_layout_hot_fields_in_first_cache_line() {
        assert_eq!(
            std::mem::size_of::<MediaPacket>(),
            56,
            "MediaPacket must remain 56 bytes"
        );

        let packet = MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0xDEAD_BEEF,
            pts: 0,
            dts: 0,
            payload: Bytes::new(),
        };
        let base = &packet as *const MediaPacket as usize;
        let media_type_offset = &packet.media_type as *const MediaType as usize - base;
        let payload_offset = &packet.payload as *const Bytes as usize - base;

        assert_eq!(media_type_offset, 0, "media_type must remain first");
        assert!(
            payload_offset >= 24,
            "payload must remain after the timestamps"
        );
        assert_eq!(std::mem::size_of::<MediaType>(), 1);
        assert_eq!(std::mem::size_of::<PayloadFormat>(), 1);
    }
}
