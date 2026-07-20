use std::hint::black_box;

use criterion::{Criterion, Throughput};
use memchr::memchr;
use restream::media::metadata::{AudioMeta, VideoMeta};
use restream::media::mpegts::{TsDemuxer, TsMuxer};

fn bench_mpegts_demux_drain(c: &mut Criterion) {
    let fixture_path =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let fixture = std::fs::read(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture_path.display()));

    let mut group = c.benchmark_group("data_path/mpegts_demux_drain");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(fixture.len() as u64));

    group.bench_function("take_then_consume", |b| {
        b.iter(|| {
            let mut demuxer = TsDemuxer::new();
            let mut consumed_bytes = 0usize;
            for chunk in fixture.chunks(1316) {
                demuxer.feed(chunk);
                for pkt in demuxer.drain() {
                    consumed_bytes += black_box(pkt.payload.len());
                }
            }
            demuxer.flush();
            for pkt in demuxer.drain() {
                consumed_bytes += black_box(pkt.payload.len());
            }
            black_box(consumed_bytes);
        });
    });

    group.bench_function("reuse_then_consume", |b| {
        b.iter(|| {
            let mut demuxer = TsDemuxer::new();
            let mut output = Vec::with_capacity(16);
            let mut consumed_bytes = 0usize;
            for chunk in fixture.chunks(1316) {
                demuxer.feed(chunk);
                demuxer.drain_into(&mut output);
                for pkt in output.drain(..) {
                    consumed_bytes += black_box(pkt.payload.len());
                }
            }
            demuxer.flush();
            demuxer.drain_into(&mut output);
            for pkt in output.drain(..) {
                consumed_bytes += black_box(pkt.payload.len());
            }
            black_box(consumed_bytes);
        });
    });

    group.finish();
}

fn bench_mpegts_resync(c: &mut Criterion) {
    let fixture_path =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let fixture = std::fs::read(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture_path.display()));

    let prefix_len = 64 * 1024;
    let mut input = vec![0u8; prefix_len];
    input.extend_from_slice(&fixture[..fixture.len().min(1316)]);

    let mut group = c.benchmark_group("data_path/mpegts_resync");
    group.throughput(Throughput::Bytes(prefix_len as u64));

    group.bench_function("memchr_sync_scan", |b| {
        b.iter(|| black_box(memchr(0x47, black_box(&input))))
    });

    group.bench_function("scalar_sync_scan", |b| {
        b.iter(|| black_box(black_box(&input).iter().position(|&b| b == 0x47)))
    });

    group.bench_function("corrupt_64k_prefix", |b| {
        b.iter(|| {
            let mut demuxer = TsDemuxer::new();
            demuxer.feed(black_box(&input));
            black_box(demuxer.has_streams());
        });
    });
    group.finish();
}

fn bench_mpegts_mux(c: &mut Criterion) {
    let fixture_path =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let fixture = std::fs::read(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture_path.display()));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&fixture);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer.take_probe();
    assert!(!packets.is_empty(), "no packets decoded from fixture");

    let video = probe.as_ref().and_then(|p| {
        p.video.as_ref().map(|v| VideoMeta {
            codec: v.codec.clone(),
            width: v.width,
            height: v.height,
            fps: v.fps,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        })
    });
    let audio_tracks: Vec<AudioMeta> = probe
        .as_ref()
        .map(|p| {
            p.audio_tracks
                .iter()
                .map(|a| AudioMeta {
                    codec: a.codec.clone(),
                    sample_rate: a.sample_rate,
                    channels: a.channels,
                    channel_layout: None,
                    track_index: a.track_index,
                    pid: None,
                    language: None,
                    title: None,
                    profile: None,
                })
                .collect()
        })
        .unwrap_or_default();

    let total_payload: usize = packets.iter().map(|p| p.payload.len()).sum();

    let mut group = c.benchmark_group("data_path/mpegts_mux");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(total_payload as u64));

    group.bench_function("mux_all_packets", |b| {
        b.iter(|| {
            let mut muxer = TsMuxer::new(video.as_ref(), &audio_tracks);
            let mut total_bytes = 0usize;
            for pkt in &packets {
                let ts = muxer.mux_packet(
                    pkt.media_type,
                    pkt.track_index,
                    pkt.pts,
                    pkt.dts,
                    pkt.is_keyframe,
                    &pkt.payload,
                );
                total_bytes += ts.len();
            }
            black_box(total_bytes)
        });
    });

    group.finish();
}

pub(super) fn register(c: &mut Criterion) {
    bench_mpegts_demux_drain(c);
    bench_mpegts_mux(c);
    bench_mpegts_resync(c);
}
