//! Shared MPEG-TS packaging for SRT subscribers and egresses.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::ingest_registry::IngestRegistry;
use crate::ts_chunk_ring::TsChunkRing;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType};
use restream_sync::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};

/// Starts the muxer task for `stage_key` on the media executor and returns
/// its TS ring, sized from `ts_ring_capacity` (`EngineConfig::ts_ring_capacity`)
/// and the source's probed packet rate. The task reads the pipeline's
/// ingest metadata and sequence headers from `ingests` and ends when the
/// ingest goes away or `cancel` fires.
pub(crate) fn start_shared_ts_muxer(
    pipeline_id: &str,
    stage_key: &str,
    source_ring: Arc<RingBuffer>,
    ingests: Arc<IngestRegistry>,
    ts_ring_capacity: usize,
    cancel: CancellationToken,
) -> Arc<TsChunkRing> {
    let ts_ring = Arc::new(TsChunkRing::new(
        srt_ts_ring_capacity(&source_ring, ts_ring_capacity),
        cancel.clone(),
    ));
    let ts_ring_clone = ts_ring.clone();
    let pipeline_id_str = pipeline_id.to_string();
    let stage_key_str = stage_key.to_string();

    let _ = crate::executor::spawn_with_class(
        crate::executor::MediaService {
            class: crate::executor::MediaServiceClass::SharedTsMux,
            pipeline_id: pipeline_id_str.clone(),
            stage: stage_key_str.clone(),
        },
        cancel.clone(),
        async move {
            let Some((video_meta, audio_tracks)) =
                wait_for_ingest_metadata(&ingests, &pipeline_id_str, &source_ring, &cancel).await
            else {
                return;
            };
            let mut mux = BurstMux::new(
                &ingests,
                &pipeline_id_str,
                &source_ring,
                video_meta.as_ref(),
                audio_tracks,
            )
            .await;
            let mut reader = Reader::new(
                format!("ts_shared_muxer:{}:{}", pipeline_id_str, stage_key_str),
                source_ring.clone(),
            );
            let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

            // Feed loop: read from source_ring, mux inline, write to ts_ring
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = reader.wait_for_data() => {
                        pull_packets.clear();
                        if let Ok(1..) = reader.pull_burst(&mut pull_packets, MEDIA_PULL_BURST_PACKETS) {
                            mux.mux_burst(&pull_packets, &reader, &ts_ring_clone);
                        }
                    }
                }
                // One bounded pull burst per media turn; an always-ready source
                // must not monopolize this worker or starve cancellation.
                tokio::task::yield_now().await;
                if !ingests.has_active_ingest(&pipeline_id_str).await {
                    break;
                }
            }
            cancel.cancel();
        },
    );

    ts_ring
}

/// Waits for the ingest's video metadata and its audio tracks (the ring's
/// routed tracks win, else the ingest's, else its one audio track). `None`
/// when cancelled, or when the ingest went away (the stage is cancelled).
async fn wait_for_ingest_metadata(
    ingests: &IngestRegistry,
    pipeline_id: &str,
    source_ring: &RingBuffer,
    cancel: &CancellationToken,
) -> Option<(Option<VideoMeta>, Arc<Vec<AudioMeta>>)> {
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        let result = ingests
            .with_active_ingest(pipeline_id, |ingest| {
                let metadata = ingest.metadata();
                let video = metadata.video;
                video.as_ref()?;
                let tracks = if let Some(routed_tracks) = source_ring.audio_tracks()
                    && !routed_tracks.is_empty()
                {
                    Arc::new(routed_tracks.to_vec())
                } else {
                    let lock = restream_sync::lock::lock(&ingest.audio_tracks);
                    if lock.is_empty()
                        && let Some(audio) = metadata.audio
                    {
                        Arc::new(vec![audio])
                    } else {
                        Arc::clone(&lock)
                    }
                };
                Some((video, tracks))
            })
            .await
            .flatten();
        if result.is_some() {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !ingests.has_active_ingest(pipeline_id).await {
            error!(
                "[srt-shared-muxer] Ingest gone while waiting for probe: {}",
                pipeline_id
            );
            cancel.cancel();
            return None;
        }
    }
}

/// The video metadata the muxer is built from: the ring's codec hint wins
/// over the ingest's codec.
fn muxer_video_meta(source_ring: &RingBuffer, video_meta: Option<&VideoMeta>) -> Option<VideoMeta> {
    let ring_codec = source_ring.codec_hint_str();
    let ingest_codec = video_meta.map(|v| v.codec.as_str()).unwrap_or("");
    let mut vm = video_meta.cloned();
    if !ring_codec.is_empty() && ring_codec != ingest_codec {
        error!(
            "[srt-shared-muxer] codec_hint override: ingest={} ring={}",
            ingest_codec, ring_codec
        );
        if let Some(ref mut v) = vm {
            v.codec = ring_codec.to_string();
        }
    }
    vm
}

/// Converts ring packets into the elementary-stream payloads TS carries,
/// reusing its buffers across packets.
struct TsPayloadConverter {
    nalu_len_size: usize,
    /// Annex B parameter sets prepended to keyframes.
    sps_pps_cache: Vec<u8>,
    video_conv_buf: Vec<u8>,
    audio_conv_buf: Vec<u8>,
}

impl TsPayloadConverter {
    /// source_ring's own cache always wins: for a preset/transcoded egress
    /// muxer, source_ring is the transcoder's output ring, which describes
    /// a different resolution/codec than the pipeline-level ingest
    /// sequence-header cache. That ingest cache is keyed only by
    /// pipeline_id (see IngestRegistry::cache_sequence_header), so it cannot
    /// distinguish "source" from "preset" — only fall back to it when the
    /// ring itself has nothing cached yet.
    async fn new(ingests: &IngestRegistry, pipeline_id: &str, source_ring: &RingBuffer) -> Self {
        let mut nalu_len_size: usize = 4;
        let sps_pps_cache = if let Some(parameter_sets) = source_ring.video_parameter_sets() {
            parameter_sets
        } else {
            match ingests.get_sequence_headers(pipeline_id).await.0 {
                Some(flv_sh) if flv_sh.len() > 5 => {
                    let (nls, annexb) = restream_core::codec::parse_avcc_config(&flv_sh[5..]);
                    nalu_len_size = nls;
                    annexb
                }
                _ => Vec::new(),
            }
        };
        Self {
            nalu_len_size,
            sps_pps_cache,
            video_conv_buf: Vec::new(),
            audio_conv_buf: Vec::new(),
        }
    }

    /// The packet's TS payload; `None` when it cannot be converted.
    fn convert<'a>(
        &'a mut self,
        pkt: &'a MediaPacket,
        source: &Reader,
        audio_tracks: &[AudioMeta],
    ) -> Option<&'a [u8]> {
        match pkt.media_type {
            MediaType::Video => {
                if self.sps_pps_cache.is_empty()
                    && let Some(parameter_sets) = source.video_parameter_sets()
                {
                    self.sps_pps_cache.extend_from_slice(&parameter_sets);
                }
                crate::codec::video_for_ts_into(
                    &pkt.payload,
                    pkt.format,
                    &mut self.nalu_len_size,
                    &mut self.sps_pps_cache,
                    &mut self.video_conv_buf,
                )
            }
            MediaType::Audio => {
                let track = audio_tracks
                    .iter()
                    .find(|a| a.track_index == pkt.track_index)
                    .or(audio_tracks.first());
                let (sr, ch) = track
                    .map(|a| (a.sample_rate, a.channels))
                    .unwrap_or((48000, 1));
                crate::codec::audio_for_ts_into(
                    &pkt.payload,
                    pkt.format,
                    sr,
                    ch,
                    &mut self.audio_conv_buf,
                )
            }
        }
    }
}

/// Muxes pull bursts into the shared TS ring: one allocation per burst,
/// sliced into one `Bytes` per muxed packet.
struct BurstMux {
    muxer: restream_mpegts::TsMuxer,
    dts_enforcer: restream_sync::DtsEnforcer,
    converter: TsPayloadConverter,
    has_video: bool,
    audio_tracks: Arc<Vec<AudioMeta>>,
    /// (byte_offset_end, media type, is_keyframe) for each muxed chunk, so
    /// a single frozen `Bytes` is sliced into per-chunk `Bytes` after the
    /// burst. This converts N malloc+memcpy calls (one per chunk) to 1
    /// malloc per burst.
    chunk_ends: Vec<(usize, MediaType, bool)>,
}

impl BurstMux {
    async fn new(
        ingests: &IngestRegistry,
        pipeline_id: &str,
        source_ring: &RingBuffer,
        video_meta: Option<&VideoMeta>,
        audio_tracks: Arc<Vec<AudioMeta>>,
    ) -> Self {
        let muxer = restream_mpegts::TsMuxer::new(
            muxer_video_meta(source_ring, video_meta).as_ref(),
            &audio_tracks,
        );
        let has_video = video_meta.is_some();
        let num_streams = usize::from(has_video) + audio_tracks.len();
        Self {
            muxer,
            dts_enforcer: restream_sync::DtsEnforcer::new(num_streams),
            converter: TsPayloadConverter::new(ingests, pipeline_id, source_ring).await,
            has_video,
            audio_tracks,
            chunk_ends: Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
        }
    }

    fn mux_burst(&mut self, packets: &[Arc<MediaPacket>], source: &Reader, ts_ring: &TsChunkRing) {
        self.chunk_ends.clear();
        // One allocation for the burst's TS output, sized to the actual
        // media payloads. A fixed 64 KiB floor pins excessive memory in the
        // retained TS ring when the muxer wakes for one small packet.
        let mut ts_accum = Vec::<u8>::with_capacity(estimate_ts_accum_capacity(packets));
        for pkt in packets {
            let Some(payload) = self.converter.convert(pkt, source, &self.audio_tracks) else {
                continue;
            };
            let stream_idx = match pkt.media_type {
                MediaType::Video => 0,
                MediaType::Audio => {
                    match self
                        .audio_tracks
                        .iter()
                        .position(|a| a.track_index == pkt.track_index)
                    {
                        Some(i) => i + usize::from(self.has_video),
                        None => continue,
                    }
                }
            };
            let (pts, dts) = self.dts_enforcer.enforce(stream_idx, pkt.pts, pkt.dts);
            // Mux directly into the burst accumulator: the muxer appends TS
            // packets to `ts_accum` with no intermediate per-packet buffer or
            // memmove into the accumulator.
            let before = ts_accum.len();
            self.muxer.mux_packet_into(
                pkt.media_type,
                pkt.track_index,
                restream_mpegts::PacketMeta {
                    pts_ms: pts,
                    dts_ms: dts,
                    is_keyframe: pkt.is_keyframe,
                },
                payload,
                &mut ts_accum,
            );
            if ts_accum.len() > before {
                self.chunk_ends
                    .push((ts_accum.len(), pkt.media_type, pkt.is_keyframe));
            }
        }
        if !self.chunk_ends.is_empty() {
            // Bytes::from(Vec) is an O(1) ownership transfer of the
            // accumulator's allocation into a shared Arc-backed Bytes.
            // slice() below only bumps the refcount — no extra allocations.
            let frozen = bytes::Bytes::from(ts_accum);
            let mut prev = 0usize;
            ts_ring.push_batch(
                self.chunk_ends
                    .drain(..)
                    .map(move |(end, media_type, is_kf)| {
                        let chunk = frozen.slice(prev..end);
                        prev = end;
                        (chunk, media_type, is_kf)
                    }),
            );
        }
    }
}

fn estimate_ts_accum_capacity(packets: &[Arc<MediaPacket>]) -> usize {
    packets
        .iter()
        .map(|packet| packet.payload.len().saturating_add(188 * 4))
        .sum::<usize>()
        .max(188)
}

/// Headroom, in seconds, the shared SRT TS ring should retain at the
/// probe-derived packet rate: one full stall-sweep interval plus the
/// TSBPD latency budget, so a leaf that misses a sweep under scheduling
/// jitter (the 131-262 ms run-queue waits measured in the scale
/// investigation) can still read without an overrun-resync — a mid-GOP
/// restart at the peer.
const SRT_TS_RING_HEADROOM_SECS: f64 = 5.0;

/// Upper bound for the derived TS ring, matching
/// `adapt_pipeline_ring`'s cap. 16,384 chunks ≈ 21 MB at 1,316 B/chunk.
const MAX_TS_RING_CAPACITY: usize = 16_384;

/// Transpose of `adapt_pipeline_ring`'s resize-on-probe pattern for the
/// shared SRT TS ring: size it at *creation* from the packet rate the
/// probe already recorded on the source ring (`set_estimated_pkt_rate`,
/// called by `adapt_pipeline_ring` after stream probe). The fixed
/// `ts_ring_capacity` default (256 chunks) is a sub-millisecond bridge at
/// MSR's real multi-track packet rate — 30 audio tracks plus video is
/// ~1,500+ pps, so 256 chunks wrap every ~0.17 s, far less than one GOP
/// interval. Returns the configured minimum when the probe hasn't set a
/// rate yet (an output attached before the pipeline ever went live);
/// in-place migration is deliberately not implemented because the muxer
/// writer task holds the ring for the pipeline's lifetime and the
/// creation-time sizing covers the realistic ordering (probe precedes
/// egress attach).
fn srt_ts_ring_capacity(source_ring: &RingBuffer, configured_capacity: usize) -> usize {
    let pkt_rate = source_ring.estimated_pkt_rate();
    if pkt_rate <= 0.0 {
        return configured_capacity.clamp(1, MAX_TS_RING_CAPACITY);
    }
    let needed = (pkt_rate * SRT_TS_RING_HEADROOM_SECS).ceil() as usize;
    needed.max(configured_capacity).min(MAX_TS_RING_CAPACITY)
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn ring_with_rate(pkt_per_sec: u32) -> RingBuffer {
        let ring = RingBuffer::new(256);
        ring.set_estimated_pkt_rate(pkt_per_sec as f64);
        ring
    }

    #[test]
    fn unprobed_ring_falls_back_to_configured_minimum() {
        // No probe rate yet (output attached before the pipeline went
        // live): keep the configured sub-millisecond-bridge default.
        let ring = RingBuffer::new(256);
        assert_eq!(srt_ts_ring_capacity(&ring, 256), 256);
        assert_eq!(srt_ts_ring_capacity(&ring, 512), 512);
    }

    #[test]
    fn msr_multitrack_rate_scales_past_the_fixed_default() {
        // 30 audio tracks (50 pps each) + 60 fps video = 1,560 pps; 5 s of
        // headroom needs 7,800 chunks — a fixed 256 would wrap every
        // ~0.17 s at this rate.
        let ring = ring_with_rate(1_560);
        assert_eq!(srt_ts_ring_capacity(&ring, 256), 7_800);
    }

    #[test]
    fn below_configured_stays_at_configured() {
        // A low-rate singletrack stream asks for less than the configured
        // minimum; the configured capacity wins.
        let ring = ring_with_rate(30);
        assert_eq!(srt_ts_ring_capacity(&ring, 256), 256);
    }

    #[test]
    fn capacity_is_capped_like_adapt_pipeline_ring() {
        let ring = ring_with_rate(5_000);
        assert_eq!(srt_ts_ring_capacity(&ring, 256), MAX_TS_RING_CAPACITY);
    }
}

#[cfg(test)]
mod tests;
