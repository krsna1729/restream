use restream_core::{MediaPacket, MediaType, NotRetained};

use super::RingBuffer;

/// Copies a source ring's video configuration into a ring that republishes
/// the source's video packets (audio router, recirculation), so the target
/// starts a generation at the packet where the source did.
#[derive(Debug, Default)]
pub struct VideoConfigFollower {
    /// Source generation last copied into the target.
    generation: u32,
}

impl VideoConfigFollower {
    /// Call before queueing the source's video packet published at
    /// `sequence` for the target. When its configuration generation differs
    /// from the one last copied, `pending` (the packets queued ahead of it)
    /// is published first and the configuration recorded. A sequence (or a
    /// generation) the source no longer retains copies the source's current
    /// configuration.
    pub fn follow(
        &mut self,
        source: &RingBuffer,
        sequence: usize,
        target: &RingBuffer,
        pending: &mut Vec<MediaPacket>,
    ) {
        let config = match source.frame_unit(sequence) {
            Some(unit) if unit.config_generation == self.generation => return,
            Some(_) => match source.track_config_at(sequence, MediaType::Video, 0) {
                Ok(config) => config,
                Err(NotRetained) => source.video_config(),
            },
            None => source.video_config(),
        };
        let Some(config) = config.filter(|config| config.generation != self.generation) else {
            return;
        };
        target.push_drained_batch_capped(pending);
        target.set_video_parameter_sets(config.bytes.to_vec());
        self.generation = config.generation;
    }
}
