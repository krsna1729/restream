//! MoQ ingest media: the tracks a publisher's catalog offers, as the
//! pipeline takes them, and each frame as the packet a TS input carries.
//!
//! The catalog maps through the shared model
//! ([`restream_moq::media::from_hang_catalog`]): at most one video
//! track (ring track 0) and every AAC track (ring indices in catalog order,
//! at most [`MAX_AUDIO_TRACKS`]). A LOC or legacy frame is a timestamp and
//! the codec payload; an empty payload is an end-of-group or discontinuity
//! marker and is skipped. Video becomes Annex B with the last parameter
//! sets ahead of a keyframe that lacks them; AAC gets the ADTS header TS
//! ingest carries; times become milliseconds. Neither carries a decode
//! time, so a track derives one that never goes back from the catalog's
//! `jitter` (its reorder bound). CMAF tracks carry `moof`+`mdat` fragments
//! and convert through the HLS-push fMP4 demux ([`super::cmaf`]), which
//! gives real decode times.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::ops::ControlFlow;

use bytes::{BufMut, Bytes, BytesMut};

use super::cmaf::CmafTrack;
use crate::ingest_publisher::fmp4::AdtsConfig;
use restream_core::codec::annexb_parameter_sets;
use restream_core::codec::build_aac_sequence_header;
use restream_core::codec::for_each_annexb_nalu;
use restream_core::codec::for_each_length_prefixed;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_moq::WireError;
use restream_moq::media::AudioConfig;
use restream_moq::media::CatalogTracks;
use restream_moq::media::TrackContainer;
use restream_moq::media::TrackKind;
use restream_moq::media::VideoCodec;
use restream_moq::media::VideoConfig;
use restream_moq::media::parse_legacy_frame;
use restream_moq::media::parse_loc_frame;

/// AAC tracks one publisher may feed; further ones are refused.
pub(crate) const MAX_AUDIO_TRACKS: usize = 8;

/// How a LOC or legacy frame carries its timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timestamped {
    Legacy,
    Loc,
}

impl Timestamped {
    /// The frame's µs timestamp and payload (no copy).
    fn split(self, frame: Bytes) -> Result<(u64, Bytes), ConvertError> {
        match self {
            Self::Legacy => parse_legacy_frame(frame),
            Self::Loc => parse_loc_frame(frame),
        }
        .map_err(ConvertError::Frame)
    }
}

/// How a video track frames its NAL units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoFraming {
    /// Behind big-endian lengths of 1..=4 bytes, as the catalog's avcC /
    /// hvcC record declares.
    LengthPrefixed(u8),
    /// Start-code delimited with the parameter sets in band (avc3 / hev1).
    AnnexB,
}

/// Why one frame was not published. The session drops it and goes on, up
/// to its malformed-frame bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvertError {
    /// The frame's timestamp or LOC properties are malformed.
    Frame(WireError),
    MalformedVideo(&'static str),
    /// A raw AAC frame longer than ADTS can describe.
    AudioFrameTooLarge(usize),
    /// A CMAF fragment the fMP4 demux refused.
    Fragment(String),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "{error}"),
            Self::MalformedVideo(reason) => f.write_str(reason),
            Self::AudioFrameTooLarge(len) => write!(f, "AAC frame of {len} bytes exceeds ADTS"),
            Self::Fragment(reason) => write!(f, "CMAF fragment: {reason}"),
        }
    }
}

/// A LOC or legacy video track's conversion state.
#[derive(Debug)]
struct VideoTrack {
    config: VideoConfig,
    timestamped: Timestamped,
    framing: VideoFraming,
    /// The last Annex-B parameter sets seen (record or keyframe).
    parameter_sets: Option<Bytes>,
    /// Probe metadata, known once parameter sets are.
    meta: Option<VideoMeta>,
    /// The catalog's reorder bound in µs.
    reorder_us: u64,
    /// The highest presentation time and last decode time handed out.
    max_pts: Option<u64>,
    last_dts: Option<u64>,
}

#[derive(Debug)]
enum TrackState {
    Video(Box<VideoTrack>),
    Aac {
        adts: AdtsConfig,
        timestamped: Timestamped,
    },
    Cmaf {
        demux: Box<CmafTrack>,
        /// The catalog's audio description, for an init without one.
        audio: Option<AudioMeta>,
    },
}

/// One accepted track and its conversion state.
#[derive(Debug)]
pub(crate) struct IngestTrack {
    pub(crate) name: String,
    pub(crate) kind: TrackKind,
    /// The ring track index: 0 for video, catalog order among AAC tracks.
    pub(crate) track_index: u32,
    state: TrackState,
}

/// The tracks a publisher's catalog offers, as the pipeline takes them.
#[derive(Debug, Default)]
pub(crate) struct TrackSet {
    tracks: Vec<IngestTrack>,
    rejected: Vec<(String, String)>,
}

impl TrackSet {
    /// Take the catalog's video track and its AAC tracks (up to
    /// [`MAX_AUDIO_TRACKS`]); everything else is kept with its reason.
    pub(crate) fn new(catalog: CatalogTracks) -> Self {
        let CatalogTracks {
            video,
            audio,
            rejected,
        } = catalog;
        let mut set = Self {
            tracks: Vec::with_capacity(audio.len().min(MAX_AUDIO_TRACKS).saturating_add(1)),
            rejected,
        };
        if let Some((name, config)) = video {
            set.take(IngestTrack::video(name, config));
        }
        let mut audio_tracks = 0u32;
        for (name, config) in audio {
            if audio_tracks as usize == MAX_AUDIO_TRACKS {
                let reason = format!("more than {MAX_AUDIO_TRACKS} audio tracks");
                set.rejected.push((name, reason));
                continue;
            }
            if set.take(IngestTrack::audio(name, config, audio_tracks)) {
                audio_tracks = audio_tracks.saturating_add(1);
            }
        }
        set
    }

    fn take(&mut self, track: Result<IngestTrack, (String, String)>) -> bool {
        match track {
            Ok(track) => {
                self.tracks.push(track);
                true
            }
            Err(rejected) => {
                self.rejected.push(rejected);
                false
            }
        }
    }

    pub(crate) fn tracks(&self) -> &[IngestTrack] {
        &self.tracks
    }

    pub(crate) fn rejected(&self) -> &[(String, String)] {
        &self.rejected
    }

    pub(crate) fn track_mut(&mut self, index: usize) -> Option<&mut IngestTrack> {
        self.tracks.get_mut(index)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// The stream probe Tokio records for the session, once it is complete:
    /// immediately without video, else once the video track knows its
    /// parameter sets (from the catalog, the init, or the first keyframe).
    pub(crate) fn probe(&self) -> Option<MoqProbe> {
        let mut video = None;
        let mut audio_tracks = Vec::new();
        for track in &self.tracks {
            match track.kind {
                TrackKind::Video => video = Some(track.video_meta()?),
                TrackKind::Audio => audio_tracks.extend(track.audio_meta()),
            }
        }
        Some(MoqProbe {
            video,
            audio_tracks,
        })
    }
}

impl IngestTrack {
    fn video(name: String, config: VideoConfig) -> Result<Self, (String, String)> {
        let state = match &config.container {
            TrackContainer::Cmaf { init } => match CmafTrack::new(init, 0) {
                Ok(demux) => TrackState::Cmaf {
                    demux: Box::new(demux),
                    audio: None,
                },
                Err(rejection) => return Err((name, rejection.message().to_string())),
            },
            TrackContainer::Legacy | TrackContainer::Loc => {
                let timestamped = if config.container == TrackContainer::Loc {
                    Timestamped::Loc
                } else {
                    Timestamped::Legacy
                };
                let (framing, parameter_sets) = match config.description {
                    None => (VideoFraming::AnnexB, None),
                    Some(_) => match config.parameter_sets() {
                        Some((sets, size)) if (1..=4).contains(&size) => {
                            (VideoFraming::LengthPrefixed(size), Some(sets))
                        }
                        _ => return Err((name, "malformed avcC/hvcC description".to_string())),
                    },
                };
                let reorder_us = config.jitter.map_or(0, |jitter| {
                    u64::try_from(jitter.as_micros()).unwrap_or(u64::MAX)
                });
                let meta = parameter_sets
                    .as_ref()
                    .map(|sets| video_meta(&config, sets));
                TrackState::Video(Box::new(VideoTrack {
                    config,
                    timestamped,
                    framing,
                    parameter_sets,
                    meta,
                    reorder_us,
                    max_pts: None,
                    last_dts: None,
                }))
            }
        };
        Ok(Self {
            name,
            kind: TrackKind::Video,
            track_index: 0,
            state,
        })
    }

    fn audio(
        name: String,
        config: AudioConfig,
        track_index: u32,
    ) -> Result<Self, (String, String)> {
        let state = match &config.container {
            TrackContainer::Cmaf { init } => match CmafTrack::new(init, track_index) {
                Ok(demux) => TrackState::Cmaf {
                    demux: Box::new(demux),
                    audio: Some(AudioMeta {
                        codec: "aac".to_string(),
                        sample_rate: config.sample_rate,
                        channels: config.channels,
                        track_index,
                        ..AudioMeta::default()
                    }),
                },
                Err(rejection) => return Err((name, rejection.message().to_string())),
            },
            TrackContainer::Legacy | TrackContainer::Loc => {
                let timestamped = if config.container == TrackContainer::Loc {
                    Timestamped::Loc
                } else {
                    Timestamped::Legacy
                };
                match aac_config(config.asc.as_deref(), config.sample_rate, config.channels) {
                    Ok(adts) => TrackState::Aac { adts, timestamped },
                    Err(reason) => return Err((name, reason)),
                }
            }
        };
        Ok(Self {
            name,
            kind: TrackKind::Audio,
            track_index,
            state,
        })
    }

    /// One frame of this track, appended to `out` as ring packets: one for a
    /// LOC or legacy frame (none for an empty one, a marker), a fragment's
    /// samples in decode order for CMAF.
    pub(crate) fn push(
        &mut self,
        frame: Bytes,
        out: &mut Vec<MediaPacket>,
    ) -> Result<(), ConvertError> {
        let track_index = self.track_index;
        match &mut self.state {
            TrackState::Cmaf { demux, .. } => demux
                .fragment(&frame, out)
                .map_err(|rejection| ConvertError::Fragment(rejection.message().to_string())),
            TrackState::Video(video) => {
                let (pts, payload) = video.timestamped.split(frame)?;
                if payload.is_empty() {
                    return Ok(());
                }
                let (keyframe, payload) = video.payload(&payload)?;
                let dts = derived_dts(
                    pts,
                    video.reorder_us,
                    &mut video.max_pts,
                    &mut video.last_dts,
                );
                out.push(MediaPacket {
                    media_type: MediaType::Video,
                    format: PayloadFormat::Raw,
                    is_keyframe: keyframe,
                    track_index,
                    pts: us_to_ms(pts),
                    dts: us_to_ms(dts),
                    payload,
                });
                Ok(())
            }
            TrackState::Aac { adts, timestamped } => {
                let (pts, payload) = timestamped.split(frame)?;
                if payload.is_empty() {
                    return Ok(());
                }
                let header = adts
                    .adts_header(payload.len())
                    .ok_or(ConvertError::AudioFrameTooLarge(payload.len()))?;
                let mut adts_frame =
                    BytesMut::with_capacity(header.len().saturating_add(payload.len()));
                adts_frame.put_slice(&header);
                adts_frame.put_slice(&payload);
                out.push(MediaPacket {
                    media_type: MediaType::Audio,
                    format: PayloadFormat::Raw,
                    is_keyframe: false,
                    track_index,
                    pts: us_to_ms(pts),
                    dts: us_to_ms(pts),
                    payload: adts_frame.freeze(),
                });
                Ok(())
            }
        }
    }

    fn video_meta(&self) -> Option<VideoMeta> {
        match &self.state {
            TrackState::Video(video) => video.meta.clone(),
            TrackState::Cmaf { demux, .. } => demux.video_meta(),
            TrackState::Aac { .. } => None,
        }
    }

    fn audio_meta(&self) -> Option<AudioMeta> {
        match &self.state {
            TrackState::Aac { adts, .. } => Some(AudioMeta {
                codec: "aac".to_string(),
                sample_rate: adts.sample_rate(),
                channels: adts.channels(),
                track_index: self.track_index,
                profile: Some(adts.profile().to_string()),
                ..AudioMeta::default()
            }),
            TrackState::Cmaf { demux, audio } => demux.audio_meta().or_else(|| audio.clone()),
            TrackState::Video(_) => None,
        }
    }
}

impl VideoTrack {
    /// One access unit as Annex B, with the known parameter sets injected
    /// ahead of a keyframe that lacks them; a keyframe that carries its own
    /// teaches them.
    fn payload(&mut self, payload: &Bytes) -> Result<(bool, Bytes), ConvertError> {
        let codec = self.config.codec;
        let scan = match self.framing {
            VideoFraming::LengthPrefixed(size) => scan_length_prefixed(codec, size, payload)?,
            VideoFraming::AnnexB => scan_annexb(codec, payload)?,
        };
        let inject = self
            .parameter_sets
            .as_ref()
            .filter(|_| scan.keyframe && !scan.has_sps);
        let annexb = match (self.framing, inject) {
            (VideoFraming::AnnexB, None) => payload.clone(),
            (framing, inject) => {
                let mut out = BytesMut::with_capacity(
                    payload
                        .len()
                        .saturating_add(payload.len() / 64)
                        .saturating_add(4)
                        .saturating_add(inject.map_or(0, Bytes::len)),
                );
                if let Some(sets) = inject {
                    out.put_slice(sets);
                }
                match framing {
                    VideoFraming::LengthPrefixed(size) => {
                        for_each_length_prefixed(usize::from(size), payload, |nalu| {
                            out.put_slice(&[0, 0, 0, 1]);
                            out.put_slice(nalu);
                        });
                    }
                    VideoFraming::AnnexB => out.put_slice(payload),
                }
                out.freeze()
            }
        };
        if scan.keyframe
            && scan.has_sps
            && let Some(sets) = annexb_parameter_sets(&annexb)
        {
            if self.meta.is_none() {
                self.meta = Some(video_meta(&self.config, &sets));
            }
            self.parameter_sets = Some(Bytes::from(sets));
        }
        Ok((scan.keyframe, annexb))
    }
}

/// Decode time for a wire without one: frames arrive in decode order and
/// trail their presentation by at most `reorder`, so the running maximum
/// presentation time less `reorder` is a decode clock that never goes back.
/// Capped at the frame's own presentation time.
fn derived_dts(pts: u64, reorder: u64, max_pts: &mut Option<u64>, last: &mut Option<u64>) -> u64 {
    let high = max_pts.map_or(pts, |max| max.max(pts));
    *max_pts = Some(high);
    let floor = last.unwrap_or(0);
    let dts = high.saturating_sub(reorder).max(floor).min(pts);
    *last = Some(dts);
    dts
}

fn video_meta(config: &VideoConfig, sets: &[u8]) -> VideoMeta {
    let mut meta = match config.codec {
        VideoCodec::H264 => restream_mpegts::h264_video_meta(sets),
        VideoCodec::H265 => restream_mpegts::hevc_video_meta(sets),
    };
    if meta.width == 0 || meta.height == 0 {
        meta.width = config.coded_width.unwrap_or_default();
        meta.height = config.coded_height.unwrap_or_default();
    }
    if meta.fps <= 0.0
        && let Some(framerate) = config.framerate.filter(|fps| fps.is_finite())
    {
        meta.fps = framerate.max(0.0);
    }
    meta
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NalScan {
    keyframe: bool,
    has_sps: bool,
}

impl NalScan {
    /// Note one NAL unit by its first header byte, codec-aware: H.264 IDR (5)
    /// and SPS (7); HEVC IRAP (16..=23) and SPS (33).
    fn note(&mut self, codec: VideoCodec, header: u8) {
        match codec {
            VideoCodec::H264 => match header & 0x1F {
                5 => self.keyframe = true,
                7 => self.has_sps = true,
                _ => {}
            },
            VideoCodec::H265 => match (header >> 1) & 0x3F {
                16..=23 => self.keyframe = true,
                33 => self.has_sps = true,
                _ => {}
            },
        }
    }
}

fn scan_length_prefixed(codec: VideoCodec, size: u8, au: &[u8]) -> Result<NalScan, ConvertError> {
    let mut scan = NalScan::default();
    let mut any = false;
    let framed = for_each_length_prefixed(usize::from(size), au, |nalu| {
        any = true;
        if let Some(&header) = nalu.first() {
            scan.note(codec, header);
        }
    });
    if !framed || !any {
        return Err(ConvertError::MalformedVideo(
            "malformed length-prefixed NAL unit",
        ));
    }
    Ok(scan)
}

fn scan_annexb(codec: VideoCodec, au: &[u8]) -> Result<NalScan, ConvertError> {
    let mut scan = NalScan::default();
    let mut any = false;
    let _ = for_each_annexb_nalu(au, |nalu| {
        any = true;
        if let Some(&header) = nalu.first() {
            scan.note(codec, header);
        }
        ControlFlow::Continue(())
    });
    if !any {
        return Err(ConvertError::MalformedVideo("no Annex-B NAL unit"));
    }
    Ok(scan)
}

/// The ADTS fields of an AAC track: from its AudioSpecificConfig when the
/// catalog carries one, else AAC-LC at the catalog's rate and channels.
pub(crate) fn aac_config(
    asc: Option<&[u8]>,
    rate: u32,
    channels: u32,
) -> Result<AdtsConfig, String> {
    match asc {
        Some(asc) => AdtsConfig::from_audio_specific_config(asc)
            .map_err(|error| format!("AAC config: {}", error.message())),
        None => build_aac_sequence_header(rate, channels)
            .get(2..)
            .and_then(|asc| AdtsConfig::from_audio_specific_config(asc).ok())
            .filter(|config| config.sample_rate() == rate && config.channels() == channels)
            .ok_or_else(|| {
                format!("AAC at {rate} Hz with {channels} channels cannot be carried as ADTS")
            }),
    }
}

/// Microseconds to milliseconds, rounded to nearest.
pub(crate) fn us_to_ms(us: u64) -> i64 {
    i64::try_from(us.saturating_add(500) / 1000).unwrap_or(i64::MAX)
}

/// The stream probe a MoQ publisher's catalog and first keyframe make.
#[derive(Debug, Clone)]
pub(crate) struct MoqProbe {
    pub(crate) video: Option<VideoMeta>,
    pub(crate) audio_tracks: Vec<AudioMeta>,
}

#[cfg(test)]
mod tests;
