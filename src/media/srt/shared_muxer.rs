//! Shared MPEG-TS packaging for SRT subscribers and egresses.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::media::engine::MediaEngine;
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use crate::media::ts_chunk_ring::TsChunkRing;

pub(crate) fn start_shared_ts_muxer(
    pipeline_id: &str,
    stage_key: &str,
    source_ring: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    cancel: CancellationToken,
) -> Arc<TsChunkRing> {
    let ts_ring = Arc::new(TsChunkRing::new(
        engine.config.ts_ring_capacity,
        cancel.clone(),
    ));
    let ts_ring_clone = ts_ring.clone();
    let pipeline_id_str = pipeline_id.to_string();
    let stage_key_str = stage_key.to_string();

    tokio::spawn(async move {
        // Wait for ingest metadata before starting the MPEG-TS muxer
        let (video_meta, audio_tracks) = loop {
            if cancel.is_cancelled() {
                return;
            }
            let result = engine
                .with_active_ingest(&pipeline_id_str, |ingest| {
                    let metadata = ingest.metadata();
                    let video = metadata.video;
                    video.as_ref()?;
                    let tracks = if let Some(routed_tracks) = source_ring.audio_tracks()
                        && !routed_tracks.is_empty()
                    {
                        std::sync::Arc::new(routed_tracks.to_vec())
                    } else {
                        let lock = ingest
                            .audio_tracks
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if lock.is_empty()
                            && let Some(audio) = metadata.audio
                        {
                            std::sync::Arc::new(vec![audio])
                        } else {
                            std::sync::Arc::clone(&lock)
                        }
                    };
                    Some((video, tracks))
                })
                .await
                .flatten();
            if let Some(meta) = result {
                break meta;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if !engine.has_active_ingest(&pipeline_id_str).await {
                error!(
                    "[srt-shared-muxer] Ingest gone while waiting for probe: {}",
                    pipeline_id_str
                );
                cancel.cancel();
                return;
            }
        };

        // Feed loop: read from source_ring, mux inline, write to ts_ring
        let muxer_video_meta = {
            let ring_codec = source_ring.codec_hint_str();
            let ingest_codec = video_meta.as_ref().map(|v| v.codec.as_str()).unwrap_or("");
            if !ring_codec.is_empty() && ring_codec != ingest_codec {
                error!(
                    "[srt-shared-muxer] codec_hint override: ingest={} ring={}",
                    ingest_codec, ring_codec
                );
                let mut vm = video_meta.clone();
                if let Some(ref mut v) = vm {
                    v.codec = ring_codec.to_string();
                }
                vm
            } else {
                video_meta.clone()
            }
        };

        let mut muxer =
            crate::media::mpegts::TsMuxer::new(muxer_video_meta.as_ref(), &audio_tracks);
        let num_streams = (video_meta.is_some() as usize) + audio_tracks.len();
        let mut dts_enforcer = crate::media::ring_buffer::DtsEnforcer::new(num_streams);
        let mut nalu_len_size: usize = 4;
        // source_ring's own cache always wins: for a preset/transcoded egress
        // muxer, source_ring is the transcoder's output ring, which describes
        // a different resolution/codec than the pipeline-level ingest
        // sequence-header cache below. That ingest cache is keyed only by
        // pipeline_id (see MediaEngine::cache_sequence_header), so it cannot
        // distinguish "source" from "preset" — only fall back to it when the
        // ring itself has nothing cached yet.
        let mut sps_pps_cache: Vec<u8> =
            if let Some(parameter_sets) = source_ring.video_parameter_sets() {
                parameter_sets.to_vec()
            } else {
                let (vsh, _) = engine.get_sequence_headers(&pipeline_id_str).await;
                if let Some(ref flv_sh) = vsh {
                    if flv_sh.len() > 5 {
                        let (nls, annexb) = crate::media::codec::parse_avcc_config(&flv_sh[5..]);
                        nalu_len_size = nls;
                        annexb
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            };

        let mut reader = Reader::new(
            format!("ts_shared_muxer:{}:{}", pipeline_id_str, stage_key_str),
            source_ring.clone(),
        );
        let mut video_conv_buf = Vec::<u8>::new();
        let mut audio_conv_buf = Vec::<u8>::new();
        // `chunk_ends` records (byte_offset_end, is_keyframe) for each muxed chunk so
        // we can slice a single frozen `Bytes` into per-chunk `Bytes` after the inner loop.
        // This converts N malloc+memcpy calls (one per chunk) to 1 malloc per burst.
        let mut chunk_ends: Vec<(usize, bool)> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
        let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = reader.wait_for_data() => {
                    pull_packets.clear();
                    match reader.pull_burst(&mut pull_packets, MEDIA_PULL_BURST_PACKETS) {
                        Ok(0) | Err(_) => {}
                        Ok(_) => {
                            chunk_ends.clear();
                            // One allocation for the burst's TS output, sized to
                            // the actual media payloads. A fixed 64 KiB floor
                            // pins excessive memory in the retained TS ring
                            // when the muxer wakes for one small packet.
                            let mut ts_accum = Vec::<u8>::with_capacity(
                                estimate_ts_accum_capacity(&pull_packets),
                            );
                            for pkt in &pull_packets {
                                let payload: &[u8] = match pkt.media_type {
                                    MediaType::Video => {
                                        if sps_pps_cache.is_empty()
                                            && let Some(parameter_sets) =
                                                reader.current_ring().video_parameter_sets()
                                        {
                                            sps_pps_cache.extend_from_slice(&parameter_sets);
                                        }
                                        match crate::media::codec::video_for_ts_into(
                                            &pkt.payload,
                                            pkt.format,
                                            &mut nalu_len_size,
                                            &mut sps_pps_cache,
                                            &mut video_conv_buf,
                                        ) {
                                            Some(p) => p,
                                            None => continue,
                                        }
                                    }
                                    MediaType::Audio => {
                                        let track = audio_tracks
                                            .iter()
                                            .find(|a| a.track_index == pkt.track_index)
                                            .or(audio_tracks.first());
                                        let (sr, ch) = track
                                            .map(|a| (a.sample_rate, a.channels))
                                            .unwrap_or((48000, 1));
                                        match crate::media::codec::audio_for_ts_into(
                                            &pkt.payload,
                                            pkt.format,
                                            sr,
                                            ch,
                                            &mut audio_conv_buf,
                                        ) {
                                            Some(p) => p,
                                            None => continue,
                                        }
                                    }
                                };

                                let stream_idx = match pkt.media_type {
                                    MediaType::Video => 0,
                                    MediaType::Audio => {
                                        let video_offset = video_meta.is_some() as usize;
                                        match audio_tracks
                                            .iter()
                                            .position(|a| a.track_index == pkt.track_index)
                                        {
                                            Some(i) => i + video_offset,
                                            None => continue,
                                        }
                                    }
                                };

                                let (pts, dts) =
                                    dts_enforcer.enforce(stream_idx, pkt.pts, pkt.dts);
                                // Mux directly into the burst accumulator: the muxer
                                // appends TS packets to `ts_accum` with no intermediate
                                // per-packet buffer or memmove into the accumulator.
                                let before = ts_accum.len();
                                muxer.mux_packet_into(
                                    pkt.media_type,
                                    pkt.track_index,
                                    crate::media::mpegts::PacketMeta {
                                        pts_ms: pts,
                                        dts_ms: dts,
                                        is_keyframe: pkt.is_keyframe,
                                    },
                                    payload,
                                    &mut ts_accum,
                                );
                                if ts_accum.len() > before {
                                    chunk_ends.push((ts_accum.len(), pkt.is_keyframe));
                                }
                            }
                            if !chunk_ends.is_empty() {
                                // Bytes::from(Vec) is an O(1) ownership transfer of the
                                // accumulator's allocation into a shared Arc-backed Bytes.
                                // slice() below only bumps the refcount — no extra allocations.
                                let frozen = bytes::Bytes::from(ts_accum);
                                let mut prev = 0usize;
                                ts_ring_clone.push_batch(chunk_ends.drain(..).map(
                                    move |(end, is_kf)| {
                                        let chunk = frozen.slice(prev..end);
                                        prev = end;
                                        (chunk, is_kf)
                                    },
                                ));
                            }
                        }
                    }
                }
            }
            if !engine
                .ingests
                .active
                .read()
                .await
                .contains_key(&pipeline_id_str)
            {
                break;
            }
        }
        cancel.cancel();
    });

    ts_ring
}

pub(super) fn estimate_ts_accum_capacity(packets: &[Arc<MediaPacket>]) -> usize {
    packets
        .iter()
        .map(|packet| packet.payload.len().saturating_add(188 * 4))
        .sum::<usize>()
        .max(188)
}

#[cfg(test)]
#[path = "shared_muxer_tests.rs"]
mod tests;
