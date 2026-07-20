use bytes::Bytes;
use restream::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, RingSlot};
use std::mem::{align_of, size_of};

pub(super) const PACKET_BYTES: usize = 1316;
pub(super) const RING_CAPACITY: usize = 4096;

pub(super) fn print_layout_baseline() {
    eprintln!(
        "data-path layout: MediaPacket={}B align={}B, RingSlot={}B align={}B, \
         {} slots={}KiB",
        size_of::<MediaPacket>(),
        align_of::<MediaPacket>(),
        size_of::<RingSlot>(),
        align_of::<RingSlot>(),
        RING_CAPACITY,
        size_of::<RingSlot>() * RING_CAPACITY / 1024,
    );
}

pub(super) fn packet(sequence: usize, payload: &Bytes) -> MediaPacket {
    MediaPacket {
        media_type: if sequence.is_multiple_of(3) {
            MediaType::Audio
        } else {
            MediaType::Video
        },
        track_index: 0,
        pts: sequence as i64 * 20,
        dts: sequence as i64 * 20,
        is_keyframe: sequence.is_multiple_of(60),
        format: PayloadFormat::Raw,
        payload: payload.clone(),
    }
}
