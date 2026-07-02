//! Benchmark: native HLS fMP4 preview cost over representative preview flows.
//!
//! This measures the two hot pieces we added for browser preview:
//! - fragmented MP4 segment metadata construction via `shiguredo_mp4`
//! - in-memory rendition publication/snapshot for video plus alternate audio

use std::num::NonZeroU32;

use bytes::Bytes;
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use restream::media::engine::{AudioMeta, VideoMeta};
use restream::media::hls::HlsConfig;
use restream::media::hls_fmp4::Fmp4HlsStore;
use shiguredo_mp4::{
    FixedPointNumber, TrackKind, Uint,
    boxes::EsdsBox,
    boxes::{
        AudioSampleEntryFields, Avc1Box, AvccBox, Mp4aBox, SampleEntry, VisualSampleEntryFields,
    },
    descriptors::{DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor},
    mux::{Fmp4SegmentMuxer, Sample},
};

const SEGMENT_SECONDS: u32 = 6;
const WINDOW_SEGMENTS: usize = 10;
const AUDIO_TRACKS: usize = 16;

fn video_meta() -> VideoMeta {
    VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    }
}

fn audio_track(index: u32) -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: if index % 2 == 0 { 2 } else { 1 },
        channel_layout: None,
        track_index: index,
        pid: None,
        language: Some(format!("lang{index}")),
        title: None,
        profile: None,
    }
}

fn video_sample_entry() -> SampleEntry {
    SampleEntry::Avc1(Avc1Box {
        visual: VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width: 1920,
            height: 1080,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        },
        avcc_box: AvccBox {
            avc_profile_indication: 66,
            profile_compatibility: 0,
            avc_level_indication: 31,
            length_size_minus_one: Uint::new(3),
            sps_list: vec![vec![0x67, 0x64, 0x00, 0x28]],
            pps_list: vec![vec![0x68, 0xEE, 0x3C, 0x80]],
            chroma_format: None,
            bit_depth_luma_minus8: None,
            bit_depth_chroma_minus8: None,
            sps_ext_list: Vec::new(),
        },
        unknown_boxes: Vec::new(),
    })
}

fn audio_sample_entry() -> SampleEntry {
    SampleEntry::Mp4a(Mp4aBox {
        audio: AudioSampleEntryFields {
            data_reference_index: AudioSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            channelcount: 2,
            samplesize: AudioSampleEntryFields::DEFAULT_SAMPLESIZE,
            samplerate: FixedPointNumber::new(48_000, 0),
        },
        esds_box: EsdsBox {
            es: EsDescriptor {
                es_id: EsDescriptor::MIN_ES_ID,
                stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
                depends_on_es_id: None,
                url_string: None,
                ocr_es_id: None,
                dec_config_descr: DecoderConfigDescriptor {
                    object_type_indication:
                        DecoderConfigDescriptor::OBJECT_TYPE_INDICATION_AUDIO_ISO_IEC_14496_3,
                    stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                    up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                    buffer_size_db: Uint::new(0),
                    max_bitrate: 0,
                    avg_bitrate: 0,
                    dec_specific_info: Some(DecoderSpecificInfo {
                        payload: vec![0x11, 0x90],
                    }),
                },
                sl_config_descr: SlConfigDescriptor,
            },
        },
        unknown_boxes: Vec::new(),
    })
}

fn build_samples(
    track_kind: TrackKind,
    sample_entry: SampleEntry,
    sample_count: usize,
    duration: u32,
    payload_len: usize,
    timescale: NonZeroU32,
) -> (Vec<Sample>, Vec<u8>) {
    let mut samples = Vec::with_capacity(sample_count);
    let mut payload = vec![0xAB; payload_len];
    if let Some(first) = payload.first_mut() {
        *first = 0x00;
    }
    let base_size = payload_len / sample_count.max(1);
    let mut remainder = payload_len % sample_count.max(1);
    let mut offset = 0u64;

    for index in 0..sample_count {
        let mut size = base_size;
        if remainder > 0 {
            size += 1;
            remainder -= 1;
        }
        samples.push(Sample {
            track_kind,
            sample_entry: Some(sample_entry.clone()),
            keyframe: track_kind == TrackKind::Audio || index % 60 == 0,
            timescale,
            duration,
            composition_time_offset: if track_kind == TrackKind::Video && index % 2 == 1 {
                Some((duration / 2) as i64)
            } else {
                None
            },
            data_offset: offset,
            data_size: size,
        });
        offset += size as u64;
    }

    (samples, payload)
}

fn build_video_segment() -> (Vec<u8>, Vec<u8>) {
    let mut muxer = Fmp4SegmentMuxer::new().expect("video muxer");
    let sample_entry = video_sample_entry();
    let (samples, payload) = build_samples(
        TrackKind::Video,
        sample_entry,
        180,
        3_000,
        3_750_000,
        NonZeroU32::new(90_000).expect("non-zero"),
    );
    let mut segment = muxer
        .create_media_segment_metadata(&samples)
        .expect("video segment metadata");
    segment.extend_from_slice(&payload);
    let init = muxer.init_segment_bytes().expect("video init");
    (init, segment)
}

fn build_audio_segment() -> (Vec<u8>, Vec<u8>) {
    let mut muxer = Fmp4SegmentMuxer::new().expect("audio muxer");
    let sample_entry = audio_sample_entry();
    let (samples, payload) = build_samples(
        TrackKind::Audio,
        sample_entry,
        282,
        1_024,
        96_000,
        NonZeroU32::new(48_000).expect("non-zero"),
    );
    let mut segment = muxer
        .create_media_segment_metadata(&samples)
        .expect("audio segment metadata");
    segment.extend_from_slice(&payload);
    let init = muxer.init_segment_bytes().expect("audio init");
    (init, segment)
}

fn bench_hls_fmp4_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("hls_fmp4_cost");
    group.sample_size(20);

    let (_video_init, video_segment) = build_video_segment();
    let (_audio_init, audio_segment) = build_audio_segment();

    group.throughput(Throughput::Bytes(video_segment.len() as u64));
    group.bench_function(BenchmarkId::new("mux_video_segment", "1080p30_h264"), |b| {
        b.iter(|| {
            let (init, segment) = build_video_segment();
            black_box(init.len() + segment.len())
        });
    });

    group.throughput(Throughput::Bytes(audio_segment.len() as u64));
    group.bench_function(BenchmarkId::new("mux_audio_segment", "aac_48khz"), |b| {
        b.iter(|| {
            let (init, segment) = build_audio_segment();
            black_box(init.len() + segment.len())
        });
    });

    let audio_tracks: Vec<AudioMeta> = (0..AUDIO_TRACKS as u32).map(audio_track).collect();
    let video_meta = video_meta();
    let video_init = Bytes::from(build_video_segment().0);
    let video_media = Bytes::from(video_segment);
    let audio_init = Bytes::from(build_audio_segment().0);
    let audio_media = Bytes::from(audio_segment);

    group.bench_function(
        BenchmarkId::new("publish_window_and_snapshot", "video_plus_16_audio"),
        |b| {
            b.iter_batched(
                || {
                    let store = Fmp4HlsStore::with_config(HlsConfig {
                        max_segments: WINDOW_SEGMENTS,
                        ..HlsConfig::default()
                    });
                    store.set_stream_metadata(Some(video_meta.clone()), audio_tracks.clone());
                    store
                },
                |store| {
                    for segment_index in 0..WINDOW_SEGMENTS as u64 {
                        store.publish_video_segment(
                            segment_index,
                            SEGMENT_SECONDS as f64,
                            video_init.clone(),
                            video_media.clone(),
                        );
                        for track in 0..AUDIO_TRACKS as u32 {
                            store.publish_audio_segment(
                                track,
                                segment_index,
                                SEGMENT_SECONDS as f64,
                                audio_init.clone(),
                                audio_media.clone(),
                            );
                        }
                    }

                    let primary = store.get_primary_playlist().expect("primary playlist");
                    let video = store.get_video_playlist().expect("video playlist");
                    let audio = store.get_audio_playlist(15).expect("audio playlist");
                    let video_seg = store.get_video_segment((WINDOW_SEGMENTS - 1) as u64);
                    let audio_seg = store.get_audio_segment(15, (WINDOW_SEGMENTS - 1) as u64);
                    black_box(
                        primary.len()
                            + video.len()
                            + audio.len()
                            + video_seg.expect("video segment").len()
                            + audio_seg.expect("audio segment").len(),
                    )
                },
                BatchSize::SmallInput,
            );
        },
    );

    group.finish();
}

criterion_group!(benches, bench_hls_fmp4_cost);
criterion_main!(benches);
