//! The sampler: one OS thread per checked pipeline. It owns a reader of the
//! source ring and waits on it off Tokio; it decodes at most one video
//! keyframe a second (an IDR decodes alone) and every packet of the first
//! audio track, and hands the results to a [`QaTracker`]. FFmpeg runs only
//! here, inside `catch_unwind`: a panic ends this pipeline's checks and
//! nothing else.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use ffmpeg_next::codec;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling::{Context as Scaler, Flags};
use ffmpeg_next::{Packet, frame};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::loudness::LoudnessMeter;
use super::picture::{THUMB_HEIGHT, THUMB_WIDTH};
use super::{QaTracker, StreamQa};
use crate::media::codec::{audio_for_ts, parse_avcc_config, video_for_ts};
use crate::media::metadata::AudioMeta;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::{Reader, RingBuffer};

/// At most one picture a second.
const PICTURE_EVERY: Duration = Duration::from_secs(1);
/// A loudness reading every half second.
const SOUND_EVERY: Duration = Duration::from_millis(500);
/// Packets taken from the ring per pull.
const BURST: usize = 256;

/// What the sampler needs from the source beside its packets; read when a
/// packet needs it, never per packet.
pub trait Source {
    /// "h264", "hevc" or "" (unknown).
    fn video_codec(&self) -> String;
    /// Annex-B parameter sets a keyframe may lack (TS and SRT sources).
    fn parameter_sets(&self) -> Option<Vec<u8>>;
    /// The RTMP sequence header (FLV tag body), for a reader that joined
    /// after it was sent.
    fn flv_sequence_header(&self) -> Option<Bytes>;
    /// The audio tracks, lowest index first.
    fn audio_tracks(&self) -> Vec<AudioMeta>;
}

/// Packets in, findings out. Holds the decoders and the tracker.
pub struct Sampler {
    tracker: QaTracker,
    dirty: bool,
    video: Option<(codec::Id, ffmpeg_next::decoder::Video)>,
    video_failed: Option<codec::Id>,
    nalu_len_size: usize,
    sps_pps: Vec<u8>,
    last_picture_ms: Option<u64>,
    scaler: Option<((Pixel, u32, u32), Scaler)>,
    decoded: frame::Video,
    scratch: frame::Video,
    grey: frame::Video,
    audio: Option<(AudioMeta, ffmpeg_next::decoder::Audio)>,
    audio_failed: bool,
    meter: Option<LoudnessMeter>,
    samples: frame::Audio,
    planes: Vec<Vec<f32>>,
    last_sound_ms: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            tracker: QaTracker::default(),
            dirty: false,
            video: None,
            video_failed: None,
            nalu_len_size: 4,
            sps_pps: Vec::new(),
            last_picture_ms: None,
            scaler: None,
            decoded: frame::Video::empty(),
            scratch: frame::Video::empty(),
            grey: frame::Video::empty(),
            audio: None,
            audio_failed: false,
            meter: None,
            samples: frame::Audio::empty(),
            planes: Vec::new(),
            last_sound_ms: 0,
        }
    }

    /// The findings, if they changed since the last call.
    pub fn take_changed(&mut self) -> Option<StreamQa> {
        std::mem::take(&mut self.dirty).then(|| self.tracker.snapshot().clone())
    }

    /// The reader skipped ahead: decoders restart at the next keyframe and
    /// the next picture is compared with nothing.
    pub fn resync(&mut self) {
        if let Some((_, decoder)) = self.video.as_mut() {
            decoder.flush();
        }
        if let Some((_, decoder)) = self.audio.as_mut() {
            decoder.flush();
        }
        self.tracker.forget_picture();
    }

    pub fn packet(&mut self, packet: &MediaPacket, source: &dyn Source, now_ms: u64) {
        match packet.media_type {
            MediaType::Video if packet.is_keyframe => self.video_keyframe(packet, source, now_ms),
            MediaType::Audio => self.audio_packet(packet, source, now_ms),
            _ => {}
        }
    }

    fn video_keyframe(&mut self, packet: &MediaPacket, source: &dyn Source, now_ms: u64) {
        let id = match source.video_codec().as_str() {
            "hevc" if packet.format == PayloadFormat::Raw => codec::Id::HEVC,
            // FLV carries H.264 here; enhanced-RTMP HEVC is not unpacked.
            "hevc" => return,
            _ => codec::Id::H264,
        };
        if self.sps_pps.is_empty() {
            match packet.format {
                PayloadFormat::Raw => {
                    if let Some(sets) = source.parameter_sets() {
                        self.sps_pps = sets;
                    }
                }
                PayloadFormat::Flv => {
                    if let Some(header) = source.flv_sequence_header()
                        && header.len() > 5
                    {
                        let (nalu_len_size, sets) = parse_avcc_config(&header[5..]);
                        if !sets.is_empty() {
                            self.nalu_len_size = nalu_len_size;
                            self.sps_pps = sets;
                        }
                    }
                }
            }
        }
        // Converted even when no picture is due: a sequence header updates
        // the cached parameter sets.
        let Some(annexb) = video_for_ts(
            &packet.payload,
            packet.format,
            &mut self.nalu_len_size,
            &mut self.sps_pps,
        ) else {
            return;
        };
        if self
            .last_picture_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < PICTURE_EVERY.as_millis() as u64)
        {
            return;
        }
        if let Some(thumb) = self.decode_thumb(id, &annexb) {
            self.last_picture_ms = Some(now_ms);
            self.tracker.picture(now_ms, thumb);
            self.dirty = true;
        }
    }

    /// One keyframe decoded alone (sent, drained, then the decoder reset
    /// for the next) and scaled to a grey thumbnail.
    fn decode_thumb(&mut self, id: codec::Id, annexb: &[u8]) -> Option<Vec<u8>> {
        if self.video_failed == Some(id) {
            return None;
        }
        if self.video.as_ref().is_none_or(|(open, _)| *open != id) {
            match open_video(id) {
                Some(decoder) => self.video = Some((id, decoder)),
                None => {
                    self.video_failed = Some(id);
                    return None;
                }
            }
        }
        let (_, decoder) = self.video.as_mut()?;
        let mut got = false;
        if decoder.send_packet(&Packet::copy(annexb)).is_ok() {
            let _ = decoder.send_eof();
            // A failed receive clears the frame it was given: receive into
            // scratch and keep the last picture that came out.
            while decoder.receive_frame(&mut self.scratch).is_ok() {
                std::mem::swap(&mut self.decoded, &mut self.scratch);
                got = true;
            }
        }
        decoder.flush();
        if !got || self.decoded.width() == 0 || self.decoded.height() == 0 {
            return None;
        }
        let shape = (
            self.decoded.format(),
            self.decoded.width(),
            self.decoded.height(),
        );
        if self.scaler.as_ref().is_none_or(|(had, _)| *had != shape) {
            let scaler = Scaler::get(
                shape.0,
                shape.1,
                shape.2,
                Pixel::GRAY8,
                THUMB_WIDTH,
                THUMB_HEIGHT,
                Flags::AREA,
            )
            .ok()?;
            self.scaler = Some((shape, scaler));
        }
        let (_, scaler) = self.scaler.as_mut()?;
        scaler.run(&self.decoded, &mut self.grey).ok()?;
        let stride = self.grey.stride(0);
        let width = THUMB_WIDTH as usize;
        let plane = self.grey.data(0);
        let mut thumb = Vec::with_capacity(width * THUMB_HEIGHT as usize);
        for row in 0..THUMB_HEIGHT as usize {
            thumb.extend_from_slice(plane.get(row * stride..row * stride + width)?);
        }
        Some(thumb)
    }

    fn audio_packet(&mut self, packet: &MediaPacket, source: &dyn Source, now_ms: u64) {
        if self.audio_failed {
            return;
        }
        // The first track, AAC: what the decoder here reads.
        let track = match &self.audio {
            Some((meta, _)) if meta.track_index == packet.track_index => meta.clone(),
            Some(_) => return,
            None => {
                let Some(meta) = source.audio_tracks().into_iter().next() else {
                    return;
                };
                if meta.track_index != packet.track_index {
                    return;
                }
                if !meta.codec.eq_ignore_ascii_case("aac") {
                    self.audio_failed = true;
                    return;
                }
                match open_audio() {
                    Some(decoder) => self.audio = Some((meta.clone(), decoder)),
                    None => {
                        self.audio_failed = true;
                        return;
                    }
                }
                meta
            }
        };
        let Some(adts) = audio_for_ts(
            &packet.payload,
            packet.format,
            track.sample_rate,
            track.channels,
        ) else {
            return;
        };
        let Some((_, decoder)) = self.audio.as_mut() else {
            return;
        };
        if decoder.send_packet(&Packet::copy(&adts)).is_err() {
            return;
        }
        while decoder.receive_frame(&mut self.samples).is_ok() {
            let rate = self.samples.rate();
            let channels = usize::from(self.samples.channels());
            if self
                .meter
                .as_ref()
                .is_none_or(|meter| meter.sample_rate() != rate || meter.channels() != channels)
            {
                self.meter = LoudnessMeter::new(rate, channels);
            }
            let Some(meter) = self.meter.as_mut() else {
                continue;
            };
            if !planar_f32(&self.samples, &mut self.planes) {
                continue;
            }
            let planes: Vec<&[f32]> = self.planes.iter().map(Vec::as_slice).collect();
            meter.push_planar(&planes);
        }
        if now_ms.saturating_sub(self.last_sound_ms) >= SOUND_EVERY.as_millis() as u64
            && let Some(lufs) = self.meter.as_ref().and_then(LoudnessMeter::short_term_lufs)
        {
            self.last_sound_ms = now_ms;
            self.tracker.sound(now_ms, lufs);
            self.dirty = true;
        }
    }
}

fn open_video(id: codec::Id) -> Option<ffmpeg_next::decoder::Video> {
    let codec = ffmpeg_next::decoder::find(id)?;
    let mut context = codec::Context::new_with_codec(codec);
    // One thread: a keyframe a second does not need a pool.
    context.set_threading(codec::threading::Config::count(1));
    context.decoder().video().ok()
}

fn open_audio() -> Option<ffmpeg_next::decoder::Audio> {
    let codec = ffmpeg_next::decoder::find(codec::Id::AAC)?;
    codec::Context::new_with_codec(codec).decoder().audio().ok()
}

/// The frame's samples as one f32 vector per channel; `false` for a sample
/// format the meter does not read.
fn planar_f32(samples: &frame::Audio, planes: &mut Vec<Vec<f32>>) -> bool {
    use ffmpeg_next::format::Sample;
    use ffmpeg_next::format::sample::Type;
    let channels = usize::from(samples.channels());
    let count = samples.samples();
    planes.resize_with(channels, Vec::new);
    let as_f32 = |bytes: &[u8]| -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    };
    match samples.format() {
        Sample::F32(Type::Planar) => {
            for (channel, plane) in planes.iter_mut().enumerate() {
                let bytes = samples.data(channel);
                let Some(bytes) = bytes.get(..count * 4) else {
                    return false;
                };
                *plane = as_f32(bytes);
            }
            true
        }
        Sample::F32(Type::Packed) => {
            let Some(bytes) = samples.data(0).get(..count * channels * 4) else {
                return false;
            };
            let interleaved = as_f32(bytes);
            for (channel, plane) in planes.iter_mut().enumerate() {
                plane.clear();
                plane.extend(interleaved.iter().skip(channel).step_by(channels));
            }
            true
        }
        _ => false,
    }
}

/// The ring and the engine, as a [`Source`].
struct RingSource {
    ring: Arc<RingBuffer>,
    sequence_header: Box<dyn Fn() -> Option<Bytes> + Send>,
}

impl Source for RingSource {
    fn video_codec(&self) -> String {
        self.ring.codec_hint_str().to_string()
    }
    fn parameter_sets(&self) -> Option<Vec<u8>> {
        self.ring.video_parameter_sets()
    }
    fn flv_sequence_header(&self) -> Option<Bytes> {
        (self.sequence_header)()
    }
    fn audio_tracks(&self) -> Vec<AudioMeta> {
        let mut tracks = self.ring.audio_tracks().unwrap_or_default();
        tracks.sort_by_key(|track| track.track_index);
        tracks
    }
}

/// Starts the sampler thread for `pipeline_id`. It stops when `cancel`
/// fires or the ring ends; findings go to `findings`.
pub fn spawn(
    pipeline_id: String,
    ring: Arc<RingBuffer>,
    sequence_header: Box<dyn Fn() -> Option<Bytes> + Send>,
    findings: Arc<Mutex<StreamQa>>,
    cancel: CancellationToken,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("stream-qa-{}", pipeline_id.chars().take(6).collect::<String>()))
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&pipeline_id, ring, sequence_header, &findings, &cancel);
            }));
            if let Err(payload) = outcome {
                let message = crate::panic_boundary::record_contained(payload.as_ref());
                warn!(pipeline_id = %pipeline_id, panic = message, "stream checks stopped by a panic");
            }
        })
}

fn run(
    pipeline_id: &str,
    ring: Arc<RingBuffer>,
    sequence_header: Box<dyn Fn() -> Option<Bytes> + Send>,
    findings: &Mutex<StreamQa>,
    cancel: &CancellationToken,
) {
    let mut reader = Reader::new(format!("qa:{pipeline_id}"), ring.clone());
    let source = RingSource {
        ring,
        sequence_header,
    };
    let mut sampler = Sampler::new();
    let mut burst = Vec::with_capacity(BURST);
    loop {
        let data = crate::media::ffmpeg::stage_input::block_on(async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => false,
                _ = reader.wait_for_data() => true,
            }
        });
        if !data || reader.is_caught_up_to_end_of_stream() {
            return;
        }
        loop {
            burst.clear();
            match reader.pull_burst(&mut burst, BURST) {
                Ok(0) => break,
                Ok(_) => {
                    let now_ms = crate::media::engine::MediaEngine::now_epoch_ms();
                    for packet in &burst {
                        sampler.packet(packet, &source, now_ms);
                    }
                }
                Err(_) => sampler.resync(),
            }
        }
        if let Some(changed) = sampler.take_changed() {
            *crate::sync::lock(findings) = changed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's codec details, as an RTMP ingest would report them.
    struct FixtureSource {
        sequence_header: Bytes,
        audio: AudioMeta,
    }

    impl Source for FixtureSource {
        fn video_codec(&self) -> String {
            "h264".to_string()
        }
        fn parameter_sets(&self) -> Option<Vec<u8>> {
            None
        }
        fn flv_sequence_header(&self) -> Option<Bytes> {
            Some(self.sequence_header.clone())
        }
        fn audio_tracks(&self) -> Vec<AudioMeta> {
            vec![self.audio.clone()]
        }
    }

    /// The fixture file's first video and first audio stream as the FLV
    /// packets an RTMP publisher sends, with their presentation time (ms).
    fn fixture_as_rtmp() -> (FixtureSource, Vec<(u64, MediaPacket)>) {
        let path = crate::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
        )
        .unwrap();
        let mut input = ffmpeg_next::format::input(&path).unwrap();
        let video = input
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .unwrap();
        let audio = input
            .streams()
            .best(ffmpeg_next::media::Type::Audio)
            .unwrap();
        let (video_index, audio_index) = (video.index(), audio.index());
        let video_base = f64::from(video.time_base());
        let audio_base = f64::from(audio.time_base());
        let parameters = video.parameters();
        // SAFETY: `parameters` holds the stream's live AVCodecParameters for
        // this whole block; FFmpeg keeps `extradata_size` readable bytes at a
        // non-null `extradata` (checked first), copied out before it ends.
        let extradata = unsafe {
            let raw = &*parameters.as_ptr();
            assert!(
                !raw.extradata.is_null(),
                "the fixture's H.264 track has an avcC record"
            );
            std::slice::from_raw_parts(raw.extradata, raw.extradata_size as usize).to_vec()
        };
        let audio_parameters = codec::Context::from_parameters(audio.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let source = FixtureSource {
            sequence_header: Bytes::from([&[0x17, 0, 0, 0, 0][..], &extradata].concat()),
            audio: AudioMeta {
                codec: "aac".to_string(),
                sample_rate: audio_parameters.rate(),
                channels: u32::from(audio_parameters.channels()),
                track_index: 0,
                ..AudioMeta::default()
            },
        };
        let mut packets = Vec::new();
        for (stream, packet) in input.packets() {
            let Some(data) = packet.data() else { continue };
            let pts = packet.pts().or(packet.dts()).unwrap_or(0);
            if stream.index() == video_index {
                let key = packet.is_key();
                let at_ms = (pts as f64 * video_base * 1000.0) as u64;
                let tag = if key { 0x17 } else { 0x27 };
                packets.push((
                    at_ms,
                    MediaPacket {
                        media_type: MediaType::Video,
                        format: PayloadFormat::Flv,
                        is_keyframe: key,
                        track_index: 0,
                        pts: at_ms as i64,
                        dts: at_ms as i64,
                        payload: Bytes::from([&[tag, 1, 0, 0, 0][..], data].concat()),
                    },
                ));
            } else if stream.index() == audio_index {
                let at_ms = (pts as f64 * audio_base * 1000.0) as u64;
                packets.push((
                    at_ms,
                    MediaPacket {
                        media_type: MediaType::Audio,
                        format: PayloadFormat::Flv,
                        is_keyframe: true,
                        track_index: 0,
                        pts: at_ms as i64,
                        dts: at_ms as i64,
                        payload: Bytes::from([&[0xAF, 1][..], data].concat()),
                    },
                ));
            }
        }
        (source, packets)
    }

    #[test]
    fn an_rtmp_style_fixture_decodes_to_a_moving_picture_and_a_loudness() {
        let (source, packets) = fixture_as_rtmp();
        let mut sampler = Sampler::new();
        let mut pictures = 0;
        for (at_ms, packet) in &packets {
            sampler.packet(packet, &source, *at_ms);
            if let Some(qa) = sampler.take_changed()
                && packet.media_type == MediaType::Video
                && qa.picture_checked_ms == Some(*at_ms)
            {
                pictures += 1;
            }
        }
        let qa = sampler.tracker.snapshot();
        assert!(pictures >= 2, "keyframes decoded: {pictures}");
        assert_eq!(qa.black_since_ms, None, "colour bars are not black");
        assert_eq!(qa.still_since_ms, None, "the burnt-in timer moves");
        let lufs = qa.loudness_lufs.expect("3 s of audio measured");
        assert!((-70.0..0.0).contains(&lufs), "loudness {lufs}");
    }

    #[test]
    fn a_skip_in_the_source_compares_the_next_picture_with_nothing() {
        let (source, packets) = fixture_as_rtmp();
        let mut sampler = Sampler::new();
        let keyframes: Vec<_> = packets
            .iter()
            .filter(|(_, packet)| packet.media_type == MediaType::Video && packet.is_keyframe)
            .collect();
        // The same keyframe twice reads as a still picture...
        let (at, first) = keyframes[0];
        sampler.packet(first, &source, *at);
        sampler.packet(first, &source, at + 2_000);
        assert_eq!(sampler.tracker.snapshot().still_since_ms, Some(*at));
        // ...but not across a resync.
        sampler.resync();
        sampler.packet(first, &source, at + 4_000);
        assert_eq!(sampler.tracker.snapshot().still_since_ms, None);
    }
}
