//! The CMAF entry point: a MoQ track whose payloads are `moof`+`mdat`
//! fragments under a catalog init segment converts through the HLS-push
//! fMP4 demux ([`Fmp4Demux`]), so real decode times, composition offsets,
//! Annex-B video and ADTS audio come out as they do for HLS push.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use bytes::Bytes;

use crate::media::ingest_publisher::fmp4::Fmp4Demux;
use crate::media::ingest_publisher::{MediaRejection, SegmentDemux};
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType};
use restream_mpegts::DemuxProbe;

/// The largest init segment a catalog may carry.
pub(crate) const MAX_CMAF_INIT_BYTES: usize = 1 << 20;

/// One CMAF track's demux. Its init's audio tracks take ring indices from
/// `audio_base` up.
pub(crate) struct CmafTrack {
    demux: Fmp4Demux,
    probe: Option<DemuxProbe>,
    audio_base: u32,
}

impl std::fmt::Debug for CmafTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CmafTrack")
            .field("audio_base", &self.audio_base)
            .finish_non_exhaustive()
    }
}

impl CmafTrack {
    pub(crate) fn new(init: &[u8], audio_base: u32) -> Result<Self, MediaRejection> {
        let mut demux = Fmp4Demux::default();
        demux.apply_init(init, MAX_CMAF_INIT_BYTES)?;
        let probe = demux.take_probe();
        Ok(Self {
            demux,
            probe,
            audio_base,
        })
    }

    pub(crate) fn video_meta(&self) -> Option<VideoMeta> {
        self.probe.as_ref().and_then(|probe| probe.video.clone())
    }

    pub(crate) fn audio_meta(&self) -> Option<AudioMeta> {
        let mut meta = self.probe.as_ref()?.audio_tracks.first()?.clone();
        meta.track_index = meta.track_index.saturating_add(self.audio_base);
        Some(meta)
    }

    /// One fragment's samples, appended to `out`. A fragment the demux
    /// refuses publishes nothing.
    pub(crate) fn fragment(
        &mut self,
        fragment: &Bytes,
        out: &mut Vec<MediaPacket>,
    ) -> Result<(), MediaRejection> {
        let demuxed = self
            .demux
            .begin_object(fragment)
            .and_then(|()| self.demux.end_object());
        if let Err(rejection) = demuxed {
            self.demux.discard_object();
            return Err(rejection);
        }
        let start = out.len();
        self.demux.drain_into(out);
        for packet in out.iter_mut().skip(start) {
            if packet.media_type == MediaType::Audio {
                packet.track_index = packet.track_index.saturating_add(self.audio_base);
            }
        }
        Ok(())
    }
}
