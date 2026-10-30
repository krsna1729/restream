//! The MoQ egress publish path for `benches/moq_publish.rs`: ring units
//! through [`RingPublisher`] (media conversion, framing, moq-net groups),
//! the per-broadcast work every egress leaf and origin broadcast does.
//!
//! The bench publishes far faster than real time, so moq-net's wall-clock
//! idle window never reclaims anything; the production cache budget
//! ([`restream_moq::budget`]) evicts inline as frames are written and
//! keeps memory bounded.

use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};
use moq_net::origin;

use crate::egress::feed::{EgressFeed, FeedCursor, ReadBudget};
use crate::egress::journal::{FeedEpoch, RingFeed};
use crate::moq::egress::media::MediaConverter;
use crate::moq::egress::publish::{HangBroadcast, Pump, RingPublisher};
use crate::moq::egress::startup::{MoqAudioStartup, MoqPublishStartup, MoqVideoStartup};
use restream_core::codec::build_avcc_sequence_header;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_moq::media::AudioConfig;
use restream_moq::media::TrackContainer;
use restream_moq::media::VideoCodec;
use restream_moq::media::VideoConfig;
use restream_sync::RingBuffer;

const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];
/// 2 s GOPs at 30 fps.
const GOP_FRAMES: i64 = 60;
const KEYFRAME_BYTES: usize = 60_000;
const DELTA_BYTES: usize = 12_000;
/// AAC-LC at 48 kHz: one 1024-sample frame every 21.33 ms.
const AUDIO_FRAMES: i64 = 94;
const AUDIO_BYTES: usize = 370;

/// How the ring units look: what the pipeline's ingest left there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingUnits {
    /// FLV tags (RTMP ingest): H.264 with 4-byte NAL lengths, raw AAC.
    Flv,
    /// Raw units (SRT/TS ingest): Annex-B H.264 with in-band parameter
    /// sets on keyframes, ADTS AAC.
    AnnexB,
}

/// One broadcast fed from a ring, as an egress leaf feeds it.
pub struct PublishBench {
    units: RingUnits,
    ring: Arc<RingBuffer>,
    feed: RingFeed,
    cursor: FeedCursor,
    publisher: RingPublisher,
    pts_ms: i64,
    _origin: (origin::Producer, origin::Driver),
}

impl PublishBench {
    /// H.264 (avcC known at startup) plus AAC, as `units`.
    pub fn new(units: RingUnits) -> Self {
        let (origin, driver, budget) = restream_moq::budget::origin();
        let video = VideoConfig::from_parameter_sets(VideoCodec::H264, PARAMETER_SETS);
        let startup = MoqPublishStartup {
            video: Some(MoqVideoStartup {
                codec: VideoCodec::H264,
                config: video,
            }),
            audio: Some(MoqAudioStartup {
                track_index: 0,
                config: AudioConfig {
                    sample_rate: 48_000,
                    channels: 2,
                    asc: Some(Bytes::from_static(&[0x11, 0x90])),
                    container: TrackContainer::default(),
                },
            }),
            ..MoqPublishStartup::default()
        };
        let broadcast =
            HangBroadcast::create(&origin, budget, "bench", true, true).expect("bench broadcast");
        let publisher = RingPublisher::new(MediaConverter::new(&startup), broadcast);
        let ring = Arc::new(RingBuffer::new(4096));
        let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
        let cursor = FeedCursor::new(feed.epoch(), 0);
        let header = build_avcc_sequence_header(PARAMETER_SETS).expect("avcC");
        if units == RingUnits::Flv {
            ring.push(video_unit(false, 0, PayloadFormat::Flv, header));
        }
        Self {
            units,
            ring,
            feed,
            cursor,
            publisher,
            pts_ms: 0,
            _origin: (origin, driver),
        }
    }

    /// Push the next GOP (2 s of video and audio units) into the ring.
    pub fn push_gop(&mut self) {
        let start = self.pts_ms;
        for index in 0..GOP_FRAMES {
            let keyframe = index == 0;
            let size = if keyframe {
                KEYFRAME_BYTES
            } else {
                DELTA_BYTES
            };
            let nal_type = if keyframe { 0x65 } else { 0x41 };
            let mut unit = BytesMut::with_capacity(size + PARAMETER_SETS.len());
            let format = match self.units {
                RingUnits::Flv => {
                    unit.put_slice(&[if keyframe { 0x17 } else { 0x27 }, 1, 0, 0, 0]);
                    unit.put_u32((size - 9) as u32);
                    PayloadFormat::Flv
                }
                RingUnits::AnnexB => {
                    if keyframe {
                        unit.put_slice(PARAMETER_SETS);
                    }
                    unit.put_slice(&[0, 0, 0, 1]);
                    PayloadFormat::Raw
                }
            };
            unit.put_u8(nal_type);
            unit.resize(unit.len() + size - 10, 0xAB);
            self.ring.push(video_unit(
                keyframe,
                start + index * 1000 / 30,
                format,
                unit.freeze(),
            ));
        }
        for index in 0..AUDIO_FRAMES {
            let mut unit = BytesMut::with_capacity(AUDIO_BYTES + 7);
            let format = match self.units {
                RingUnits::Flv => {
                    unit.put_slice(&[0xAF, 1]);
                    PayloadFormat::Flv
                }
                RingUnits::AnnexB => {
                    unit.put_slice(&adts_header(AUDIO_BYTES + 7));
                    PayloadFormat::Raw
                }
            };
            unit.resize(unit.len() + AUDIO_BYTES, 0x21);
            self.ring.push(MediaPacket {
                media_type: MediaType::Audio,
                track_index: 0,
                pts: start + index * 1024 * 1000 / 48_000,
                dts: start + index * 1024 * 1000 / 48_000,
                is_keyframe: false,
                format,
                payload: unit.freeze(),
            });
        }
        self.pts_ms = start + 2000;
    }

    /// Publish everything the ring holds past the cursor; returns the frame
    /// bytes written.
    pub fn pump(&mut self) -> usize {
        let budget = ReadBudget::new(4096, 64 << 20);
        let mut written = 0;
        loop {
            match self.publisher.pump(&self.feed, &mut self.cursor, budget) {
                Pump::Sent { bytes, .. } => written += bytes,
                Pump::Empty => return written,
                other => panic!("bench publish failed: {other:?}"),
            }
        }
    }
}

fn video_unit(keyframe: bool, pts: i64, format: PayloadFormat, payload: Bytes) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts: pts,
        is_keyframe: keyframe,
        format,
        payload,
    }
}

/// An AAC-LC, 48 kHz, stereo ADTS header for a `len`-byte frame.
fn adts_header(len: usize) -> [u8; 7] {
    [
        0xFF,
        0xF1,
        0x4C,
        0x80 | (len >> 11) as u8,
        (len >> 3) as u8,
        ((len & 7) << 5) as u8 | 0x1F,
        0xFC,
    ]
}
