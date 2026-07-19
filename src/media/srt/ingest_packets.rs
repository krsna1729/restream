use std::sync::{Arc, Mutex};

use crate::media::engine::IngestRegistration;
use crate::media::input_gate::{InputPacketBoundary, InputTimestampMapper};
use crate::media::ring_buffer::{MediaPacket, MediaType, RingBuffer};

pub(super) fn forward_ingest_packets(
    packets: &mut Vec<MediaPacket>,
    ring: &RingBuffer,
    registration: &IngestRegistration,
    timestamp_mapper: &mut InputTimestampMapper,
    keyframe_times: Option<&Arc<Mutex<Vec<i64>>>>,
) {
    if let Some(preview_ring) = registration.preview_ring.load_full() {
        update_parameter_sets(&preview_ring, packets);
        preview_ring.push_batch(packets.iter().cloned());
    }

    let first_keyframe = packets
        .iter()
        .position(|packet| packet.media_type == MediaType::Video && packet.is_keyframe);
    let boundary = if first_keyframe.is_some() {
        InputPacketBoundary::VideoKeyframe
    } else {
        InputPacketBoundary::Other
    };
    let Some(lease) = registration.gate.try_enter(boundary) else {
        packets.clear();
        return;
    };

    if lease.activated()
        && let Some(first_keyframe) = first_keyframe
    {
        packets.drain(..first_keyframe);
    }
    for packet in packets.iter_mut() {
        timestamp_mapper.map_packet(packet, lease.activated(), &registration.last_forwarded_dts);
    }
    update_parameter_sets(ring, packets);
    if let Some(keyframe_times) = keyframe_times {
        record_keyframes(keyframe_times, packets);
    }
    if let Some(last) = packets.iter().max_by_key(|packet| packet.dts) {
        InputTimestampMapper::record_forwarded(last, &registration.last_forwarded_dts);
    }
    ring.push_drained_batch_capped(packets);
}

fn update_parameter_sets(ring: &RingBuffer, packets: &[MediaPacket]) {
    for packet in packets {
        if packet.media_type == MediaType::Video
            && let Some(parameter_sets) =
                crate::media::codec::annexb_parameter_sets(&packet.payload)
        {
            ring.set_video_parameter_sets(parameter_sets);
        }
    }
}

fn record_keyframes(keyframe_times: &Arc<Mutex<Vec<i64>>>, packets: &[MediaPacket]) {
    for packet in packets {
        if packet.media_type != MediaType::Video || !packet.is_keyframe {
            continue;
        }
        let mut times = keyframe_times
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        times.push(packet.pts);
        if times.len() > 30 {
            times.remove(0);
        }
    }
}
