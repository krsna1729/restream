//! Stable packet vocabulary shared by media producers, transforms, and consumers.
//!
//! These types describe encoded media independently of the transport or buffer
//! carrying it. Keep this module dependency-light so it can become a crate
//! boundary if its API and compile-time isolation prove useful later.

use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MediaType {
    Video = 0,
    Audio = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PayloadFormat {
    Flv = 0,
    Raw = 1,
}

/// 56-byte media packet. `#[repr(C)]` pins the field order so the declared
/// layout is always respected, preventing the compiler from reordering fields
/// into a layout that scatters hot fields across two cache lines.
///
/// Without `#[repr(C)]`, rustc's default greedy-alignment algorithm places the
/// largest field (`payload: Bytes`, 32 bytes) first within the struct. That puts
/// `media_type`, `is_keyframe`, and `pts`/`dts` at offsets 52–63 inside
/// `ArcInner`, spanning two 64-byte cache lines.
///
/// With the declared field order the `ArcInner<MediaPacket>` layout is:
///
/// ```text
/// Byte  0– 7  strong refcount
/// Byte  8–15  weak refcount
/// Byte 16     media_type
/// Byte 17     format
/// Byte 18     is_keyframe
/// Byte 19     padding
/// Byte 20–23  track_index
/// Byte 24–31  pts
/// Byte 32–39  dts
/// Byte 40–47  payload.ptr
/// Byte 48–55  payload.len
/// Byte 56–63  payload.data
/// Byte 64–71  payload.vtable
/// ```
///
/// Type dispatch, track routing, timestamps, and the payload pointer and length
/// therefore fit in the first cache line of `ArcInner<MediaPacket>`.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct MediaPacket {
    pub media_type: MediaType,
    pub format: PayloadFormat,
    pub is_keyframe: bool,
    pub track_index: u32,
    pub pts: i64,
    pub dts: i64,
    pub payload: Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

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
