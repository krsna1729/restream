//! Phase 6a exit gate: "the accepted [recirculation] implementation must
//! be cheaper than routing through a loopback network output and input
//! for the same compatible media path"
//! (`docs/egress-implementation.md`).
//!
//! Measures the one cost recirculation (`RecirculationInputPublisher`,
//! `src/media/recirculation.rs`) structurally cannot pay and a loopback
//! network path structurally cannot avoid: RTMP wire-chunk encoding.
//! Recirculation publishes the source pipeline's already-decoded
//! `MediaPacket`s directly into the target pipeline's ring — no wire
//! protocol touches the data at all. A loopback RTMP output+input pair
//! must serialize every packet into RTMP chunks to send it, then
//! deserialize those chunks back out on the receiving side, *in addition
//! to* whatever ring-publish work both paths share. This benchmark
//! isolates and measures that mandatory extra encoding cost using the
//! same `rml_rtmp` chunk serializer restream's own RTMP egress path uses
//! internally (that internal call site is `pub(crate)`, not reachable
//! from a bench target, so this uses the public `rml_rtmp` API directly
//! — the same representative-reimplementation approach
//! `benches/rtmp_serializer.rs` already takes for the same reason).
//!
//! Real socket I/O (the send/recv syscalls a loopback path also pays) is
//! not measured here — see `docs/egress-implementation.md` Phase 5's SRT
//! profiling writeup for why that cost is real but a fabric-layer
//! benchmark cannot usefully isolate it from kernel/NIC-loopback
//! variance. The wire-encoding cost measured here is the part that is
//! deterministic, attributable to this codebase, and unavoidable for any
//! network path — recirculation's cost is a strict subset of it.

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rml_rtmp::chunk_io::ChunkSerializer;
use rml_rtmp::messages::{MessagePayload, RtmpMessage};
use rml_rtmp::time::RtmpTimestamp;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use restream::media::engine::IngestRegistration;
use restream::media::input_gate::InputPacketGate;
use restream::media::packet::{MediaPacket, MediaType, PayloadFormat};
use restream::media::recirculation::RecirculationInputPublisher;
use restream::media::ring_buffer::RingBuffer;

fn make_packets(count: usize, payload_bytes: usize) -> Vec<Arc<MediaPacket>> {
    (0..count)
        .map(|index| {
            Arc::new(MediaPacket {
                media_type: MediaType::Video,
                format: PayloadFormat::Raw,
                is_keyframe: index == 0,
                track_index: 0,
                pts: index as i64 * 33,
                dts: index as i64 * 33,
                payload: Bytes::from(vec![0x42u8; payload_bytes]),
            })
        })
        .collect()
}

fn registration() -> IngestRegistration {
    IngestRegistration {
        cancel_token: tokio_util::sync::CancellationToken::new(),
        attempt_id: 1,
        input_id: "bench-target".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    }
}

/// One RTMP chunk-serialize round for one packet — the mandatory extra
/// work a loopback RTMP path pays that recirculation skips entirely.
fn rtmp_encode_one(serializer: &mut ChunkSerializer, payload: &Bytes, timestamp: u32) {
    let message = RtmpMessage::VideoData {
        data: payload.clone(),
    };
    let payload = MessagePayload::from_rtmp_message(message, RtmpTimestamp::new(timestamp), 1)
        .expect("valid RTMP message payload");
    let packet = serializer
        .serialize(&payload, false, true)
        .expect("valid RTMP chunk serialization");
    black_box(packet);
}

fn bench_recirculation_vs_rtmp_wire_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("recirculation_vs_loopback_wire_cost");

    for payload_bytes in [188usize, 1_316, 8_192] {
        group.throughput(Throughput::Bytes(payload_bytes as u64));

        group.bench_with_input(
            BenchmarkId::new("recirculation_publish", payload_bytes),
            &payload_bytes,
            |b, &payload_bytes| {
                let packets = make_packets(32, payload_bytes);
                let target_ring = RingBuffer::new(64);
                let registration = registration();
                b.iter_batched(
                    RecirculationInputPublisher::default,
                    |mut publisher| {
                        let outcome = publisher.publish(&packets, &target_ring, &registration);
                        black_box(outcome);
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("loopback_rtmp_wire_encode_only", payload_bytes),
            &payload_bytes,
            |b, &payload_bytes| {
                let packets = make_packets(32, payload_bytes);
                b.iter_batched(
                    ChunkSerializer::new,
                    |mut serializer| {
                        for (index, packet) in packets.iter().enumerate() {
                            rtmp_encode_one(&mut serializer, &packet.payload, index as u32 * 33);
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new(
                "recirculation_publish_plus_loopback_wire_encode",
                payload_bytes,
            ),
            &payload_bytes,
            |b, &payload_bytes| {
                let packets = make_packets(32, payload_bytes);
                let target_ring = RingBuffer::new(64);
                let registration = registration();
                b.iter_batched(
                    || {
                        (
                            RecirculationInputPublisher::default(),
                            ChunkSerializer::new(),
                        )
                    },
                    |(mut publisher, mut serializer)| {
                        // What a loopback network path would cost on top
                        // of the same ring-publish work: encode every
                        // packet to RTMP chunks *in addition to*
                        // publishing it, since the sender side still owns
                        // its own local pipeline state independent of
                        // what it sends over the wire.
                        for (index, packet) in packets.iter().enumerate() {
                            rtmp_encode_one(&mut serializer, &packet.payload, index as u32 * 33);
                        }
                        let outcome = publisher.publish(&packets, &target_ring, &registration);
                        black_box(outcome);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_recirculation_vs_rtmp_wire_encode);
criterion_main!(benches);
