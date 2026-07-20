use crate::media::packet::{MediaPacket, MediaType};

pub const DEFAULT_STANDBY_GOP_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_STANDBY_GOP_MAX_PACKETS: usize = 2_048;

#[derive(Debug)]
pub struct StandbyGopCache {
    packets: Vec<MediaPacket>,
    payload_bytes: usize,
    max_payload_bytes: usize,
    max_packets: usize,
}

impl Default for StandbyGopCache {
    fn default() -> Self {
        Self::new(
            DEFAULT_STANDBY_GOP_MAX_BYTES,
            DEFAULT_STANDBY_GOP_MAX_PACKETS,
        )
    }
}

impl StandbyGopCache {
    pub fn new(max_payload_bytes: usize, max_packets: usize) -> Self {
        Self {
            packets: Vec::new(),
            payload_bytes: 0,
            max_payload_bytes,
            max_packets,
        }
    }

    pub fn push(&mut self, packet: MediaPacket) {
        if packet.media_type == MediaType::Video && packet.is_keyframe {
            self.clear();
        } else if self.packets.is_empty() {
            return;
        }

        let next_bytes = self.payload_bytes.saturating_add(packet.payload.len());
        if next_bytes > self.max_payload_bytes || self.packets.len() >= self.max_packets {
            self.clear();
            return;
        }

        self.payload_bytes = next_bytes;
        self.packets.push(packet);
    }

    pub fn is_replay_ready(&self) -> bool {
        self.packets
            .first()
            .is_some_and(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
    }

    pub fn take_replay(&mut self) -> Vec<MediaPacket> {
        if !self.is_replay_ready() {
            return Vec::new();
        }
        self.payload_bytes = 0;
        std::mem::take(&mut self.packets)
    }

    pub fn packets(&self) -> &[MediaPacket] {
        &self.packets
    }

    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }

    pub fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.payload_bytes = 0;
    }
}
