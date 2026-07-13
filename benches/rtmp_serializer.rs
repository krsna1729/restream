use bytes::Bytes;
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use rml_rtmp::chunk_io::ChunkSerializer;
use rml_rtmp::messages::MessagePayload;
use rml_rtmp::time::RtmpTimestamp;
use std::collections::HashMap;

const MAX_INITIAL_TIMESTAMP: u32 = 16_777_215;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderFormat {
    Full,
    TimeDeltaWithoutMessageStreamId,
    TimeDeltaOnly,
    Empty,
}

#[derive(Clone, Debug)]
struct Header {
    chunk_stream_id: u32,
    timestamp: RtmpTimestamp,
    timestamp_field: u32,
    message_type_id: u8,
    message_stream_id: u32,
    message_length: u32,
    can_be_dropped: bool,
}

struct DirectChunkSerializer {
    previous_headers: HashMap<u32, Header>,
    max_chunk_size: usize,
}

impl DirectChunkSerializer {
    fn new(max_chunk_size: usize) -> Self {
        Self {
            previous_headers: HashMap::new(),
            max_chunk_size,
        }
    }

    fn serialize_into(
        &mut self,
        message: &MessagePayload,
        force_uncompressed: bool,
        can_be_dropped: bool,
        output: &mut Vec<u8>,
    ) {
        output.clear();
        output.reserve(serialized_capacity_hint(
            message.data.len(),
            self.max_chunk_size,
        ));

        let mut offset = 0;
        let mut continued = false;
        while offset < message.data.len() {
            let end = (offset + self.max_chunk_size).min(message.data.len());
            self.add_chunk(
                output,
                message,
                force_uncompressed,
                continued,
                &message.data[offset..end],
                can_be_dropped,
            );
            offset = end;
            continued = true;
        }
    }

    fn add_chunk(
        &mut self,
        output: &mut Vec<u8>,
        message: &MessagePayload,
        force_uncompressed: bool,
        continued_chunk: bool,
        data_to_write: &[u8],
        can_be_dropped: bool,
    ) {
        let mut header = Header {
            chunk_stream_id: get_csid_for_message_type(message.type_id),
            timestamp: message.timestamp,
            timestamp_field: 0,
            message_type_id: message.type_id,
            message_stream_id: message.message_stream_id,
            message_length: message.data.len() as u32,
            can_be_dropped,
        };

        let header_format = if force_uncompressed {
            HeaderFormat::Full
        } else {
            match self.previous_headers.get(&header.chunk_stream_id) {
                None => HeaderFormat::Full,
                Some(previous) => {
                    if continued_chunk {
                        header.timestamp_field = previous.timestamp_field;
                        HeaderFormat::Empty
                    } else if previous.can_be_dropped {
                        HeaderFormat::Full
                    } else {
                        header.timestamp_field = header.timestamp.value - previous.timestamp.value;
                        get_header_format(&header, previous)
                    }
                }
            }
        };

        if header_format == HeaderFormat::Full {
            header.timestamp_field = header.timestamp.value;
        }

        add_basic_header(output, header_format, header.chunk_stream_id);
        add_initial_timestamp(output, header_format, &header);
        add_message_length_and_type_id(
            output,
            header_format,
            header.message_length,
            header.message_type_id,
        );
        add_message_stream_id(output, header_format, header.message_stream_id);
        add_extended_timestamp(output, &header);
        output.extend_from_slice(data_to_write);

        self.previous_headers.insert(header.chunk_stream_id, header);
    }
}

fn serialized_capacity_hint(payload_len: usize, chunk_size: usize) -> usize {
    let chunks = payload_len.div_ceil(chunk_size.max(1));
    payload_len + chunks * 18
}

fn add_basic_header(output: &mut Vec<u8>, format: HeaderFormat, csid: u32) {
    let format_mask = match format {
        HeaderFormat::Full => 0b0000_0000,
        HeaderFormat::TimeDeltaWithoutMessageStreamId => 0b0100_0000,
        HeaderFormat::TimeDeltaOnly => 0b1000_0000,
        HeaderFormat::Empty => 0b1100_0000,
    };
    output.push((csid as u8) | format_mask);
}

fn add_initial_timestamp(output: &mut Vec<u8>, format: HeaderFormat, header: &Header) {
    if format == HeaderFormat::Empty {
        return;
    }
    push_u24_be(output, header.timestamp_field.min(MAX_INITIAL_TIMESTAMP));
}

fn add_message_length_and_type_id(
    output: &mut Vec<u8>,
    format: HeaderFormat,
    length: u32,
    type_id: u8,
) {
    if matches!(format, HeaderFormat::Empty | HeaderFormat::TimeDeltaOnly) {
        return;
    }
    push_u24_be(output, length);
    output.push(type_id);
}

fn add_message_stream_id(output: &mut Vec<u8>, format: HeaderFormat, stream_id: u32) {
    if format != HeaderFormat::Full {
        return;
    }
    output.extend_from_slice(&stream_id.to_le_bytes());
}

fn add_extended_timestamp(output: &mut Vec<u8>, header: &Header) {
    if header.timestamp_field >= MAX_INITIAL_TIMESTAMP {
        output.extend_from_slice(&header.timestamp_field.to_be_bytes());
    }
}

fn push_u24_be(output: &mut Vec<u8>, value: u32) {
    output.push(((value >> 16) & 0xff) as u8);
    output.push(((value >> 8) & 0xff) as u8);
    output.push((value & 0xff) as u8);
}

fn get_csid_for_message_type(message_type_id: u8) -> u32 {
    match message_type_id {
        1..=6 => 2,
        18 | 19 => 3,
        9 => 4,
        8 => 5,
        _ => 6,
    }
}

fn get_header_format(current: &Header, previous: &Header) -> HeaderFormat {
    if current.message_stream_id != previous.message_stream_id {
        return HeaderFormat::Full;
    }
    if current.message_type_id != previous.message_type_id
        || current.message_length != previous.message_length
    {
        return HeaderFormat::TimeDeltaWithoutMessageStreamId;
    }
    if current.timestamp_field != previous.timestamp_field {
        return HeaderFormat::TimeDeltaOnly;
    }
    HeaderFormat::Empty
}

fn media_payload(type_id: u8, size: usize, timestamp: u32) -> MessagePayload {
    MessagePayload {
        timestamp: RtmpTimestamp::new(timestamp),
        type_id,
        message_stream_id: 1,
        data: Bytes::from(vec![0x55; size]),
    }
}

fn message_sequence(payload_size: usize, type_id: u8) -> Vec<MessagePayload> {
    (0..64)
        .map(|index| media_payload(type_id, payload_size, 33 * index))
        .collect()
}

fn assert_direct_matches_rml(messages: &[MessagePayload], chunk_size: u32) {
    let mut rml = ChunkSerializer::new();
    let mut direct = DirectChunkSerializer::new(chunk_size as usize);
    rml.set_max_chunk_size(chunk_size, RtmpTimestamp::new(0))
        .unwrap();
    let mut direct_out = Vec::new();

    for message in messages {
        let expected = rml.serialize(message, false, false).unwrap();
        direct.serialize_into(message, false, false, &mut direct_out);
        assert_eq!(direct_out, expected.bytes);
    }
}

fn bench_rtmp_serializer(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtmp/serializer");

    for (label, type_id, payload_size) in [
        ("audio_200b", 8, 200),
        ("video_pframe_8k", 9, 8 * 1024),
        ("video_pframe_30k", 9, 30 * 1024),
        ("video_idr_80k", 9, 80 * 1024),
    ] {
        let messages = message_sequence(payload_size, type_id);
        for chunk_size in [4096_u32, 16 * 1024, 64 * 1024] {
            let case_label = format!("{label}_chunk{chunk_size}");
            assert_direct_matches_rml(&messages, chunk_size);
            group.throughput(Throughput::Bytes(payload_size as u64));

            group.bench_with_input(
                BenchmarkId::new("rml_owned_vec", &case_label),
                &messages,
                |b, messages| {
                    b.iter_batched(
                        || {
                            let mut serializer = ChunkSerializer::new();
                            serializer
                                .set_max_chunk_size(chunk_size, RtmpTimestamp::new(0))
                                .unwrap();
                            serializer
                        },
                        |mut serializer| {
                            for message in messages {
                                black_box(serializer.serialize(message, false, false).unwrap());
                            }
                        },
                        BatchSize::SmallInput,
                    )
                },
            );

            group.bench_with_input(
                BenchmarkId::new("direct_into_reused_vec", &case_label),
                &messages,
                |b, messages| {
                    b.iter_batched(
                        || {
                            let serializer = DirectChunkSerializer::new(chunk_size as usize);
                            let output = Vec::with_capacity(serialized_capacity_hint(
                                payload_size,
                                chunk_size as usize,
                            ));
                            (serializer, output)
                        },
                        |(mut serializer, mut output)| {
                            for message in messages {
                                serializer.serialize_into(message, false, false, &mut output);
                                black_box(&output);
                            }
                        },
                        BatchSize::SmallInput,
                    )
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_rtmp_serializer);
criterion_main!(benches);
