use bytes::Bytes;
use rml_rtmp::time::RtmpTimestamp;

const MAX_CHUNKS_PER_IOV: usize = 8;
const MAX_HEADER_BYTES: usize = 16;

/// A media RTMP message whose headers live in fixed storage and whose payload
/// remains owned by the encoder. The first chunk is always type 0 and
/// continuations are type 3, so this does not depend on rml_rtmp's private
/// header-compression state.
pub(super) struct RtmpWireMessage {
    payload: Bytes,
    chunk_size: usize,
    chunk_count: usize,
    chunk_index: usize,
    payload_offset: usize,
    timestamp: u32,
    type_id: u8,
    message_stream_id: u32,
    header_offsets: [usize; MAX_CHUNKS_PER_IOV],
    header_lengths: [usize; MAX_CHUNKS_PER_IOV],
    headers: [[u8; MAX_HEADER_BYTES]; MAX_CHUNKS_PER_IOV],
}

impl RtmpWireMessage {
    pub(super) fn new(
        type_id: u8,
        timestamp: RtmpTimestamp,
        message_stream_id: u32,
        payload: Bytes,
        chunk_size: usize,
    ) -> Result<Self, &'static str> {
        if chunk_size == 0 {
            return Err("RTMP chunk size must be non-zero");
        }
        if payload.len() > 0x00ff_ffff {
            return Err("RTMP media message exceeds the protocol length limit");
        }

        let chunk_count = payload.len().div_ceil(chunk_size).max(1);
        let mut message = Self {
            payload,
            chunk_size,
            chunk_count,
            chunk_index: 0,
            payload_offset: 0,
            timestamp: timestamp.value,
            type_id,
            message_stream_id,
            header_offsets: [0; MAX_CHUNKS_PER_IOV],
            header_lengths: [0; MAX_CHUNKS_PER_IOV],
            headers: [[0; MAX_HEADER_BYTES]; MAX_CHUNKS_PER_IOV],
        };
        message.write_headers_from_current();
        Ok(message)
    }

    pub(super) fn remaining_len(&self) -> usize {
        if self.is_complete() {
            return 0;
        }
        let header_remaining = self.header_lengths[0] - self.header_offsets[0];
        let current_payload_remaining = self.current_payload_len() - self.payload_offset;
        let later_payload = self
            .payload
            .len()
            .saturating_sub((self.chunk_index + 1) * self.chunk_size);
        let later_headers = self
            .chunk_count
            .saturating_sub(self.chunk_index + 1)
            .saturating_mul(header_len(false, self.timestamp));
        header_remaining
            .saturating_add(current_payload_remaining)
            .saturating_add(later_payload)
            .saturating_add(later_headers)
    }

    pub(super) fn is_complete(&self) -> bool {
        self.chunk_index >= self.chunk_count
    }

    pub(super) fn fill_buffers<'a>(
        &'a mut self,
        max_bytes: usize,
        buffers: &mut [&'a [u8]; MAX_CHUNKS_PER_IOV * 2],
    ) -> (usize, usize) {
        if self.is_complete() || max_bytes == 0 {
            return (0, 0);
        }
        let mut count = 0;
        let mut bytes = 0;
        let mut chunk = self.chunk_index;
        while chunk < self.chunk_count && count + 1 < buffers.len() && bytes < max_bytes {
            let header_index = chunk - self.chunk_index;
            let header_offset = if header_index == 0 {
                self.header_offsets[0]
            } else {
                0
            };
            let header_len = self.header_lengths[header_index];
            let header_bytes = (header_len - header_offset).min(max_bytes - bytes);
            buffers[count] =
                &self.headers[header_index][header_offset..header_offset + header_bytes];
            count += 1;
            bytes += header_bytes;
            if header_bytes < header_len - header_offset || bytes == max_bytes {
                break;
            }

            let payload_start = chunk * self.chunk_size
                + if header_index == 0 {
                    self.payload_offset
                } else {
                    0
                };
            let payload_len = self.payload.len().saturating_sub(payload_start).min(
                self.chunk_size.saturating_sub(if header_index == 0 {
                    self.payload_offset
                } else {
                    0
                }),
            );
            let payload_bytes = payload_len.min(max_bytes - bytes);
            if payload_bytes != 0 {
                buffers[count] = &self.payload[payload_start..payload_start + payload_bytes];
                count += 1;
                bytes += payload_bytes;
            }
            if payload_bytes < payload_len {
                break;
            }
            chunk += 1;
        }
        (count, bytes)
    }

    pub(super) fn consume(&mut self, mut bytes: usize) {
        while bytes != 0 && !self.is_complete() {
            let header_remaining = self.header_lengths[0] - self.header_offsets[0];
            if header_remaining != 0 {
                let consumed = bytes.min(header_remaining);
                self.header_offsets[0] += consumed;
                bytes -= consumed;
                continue;
            }

            let payload_remaining = self.current_payload_len() - self.payload_offset;
            if payload_remaining == 0 {
                self.chunk_index += 1;
                self.payload_offset = 0;
                if !self.is_complete() {
                    self.write_headers_from_current();
                }
                continue;
            }
            let consumed = bytes.min(payload_remaining);
            self.payload_offset += consumed;
            bytes -= consumed;
            if self.payload_offset == self.current_payload_len() {
                self.chunk_index += 1;
                self.payload_offset = 0;
                if !self.is_complete() {
                    self.write_headers_from_current();
                }
            }
        }
    }

    fn current_payload_len(&self) -> usize {
        self.payload
            .len()
            .saturating_sub(self.chunk_index * self.chunk_size)
            .min(self.chunk_size)
    }

    fn write_headers_from_current(&mut self) {
        self.header_offsets = [0; MAX_CHUNKS_PER_IOV];
        for index in 0..MAX_CHUNKS_PER_IOV {
            let chunk = self.chunk_index + index;
            if chunk >= self.chunk_count {
                self.header_lengths[index] = 0;
                continue;
            }
            let full = chunk == 0;
            let csid = self.csid();
            let header = &mut self.headers[index];
            let mut length = 0;
            header[length] = if full { csid } else { 0xc0 | csid };
            length += 1;
            if full {
                write_u24(&mut header[length..], self.timestamp.min(0x00ff_ffff));
                length += 3;
                write_u24(&mut header[length..], self.payload.len() as u32);
                length += 3;
                header[length] = self.type_id;
                length += 1;
                header[length..length + 4].copy_from_slice(&self.message_stream_id.to_le_bytes());
                length += 4;
            }
            if self.timestamp >= 0x00ff_ffff {
                header[length..length + 4].copy_from_slice(&self.timestamp.to_be_bytes());
                length += 4;
            }
            self.header_lengths[index] = length;
        }
    }

    fn csid(&self) -> u8 {
        match self.type_id {
            8 => 5,
            9 => 4,
            _ => 6,
        }
    }
}

fn header_len(full: bool, timestamp: u32) -> usize {
    (if full { 12 } else { 1 }) + usize::from(timestamp >= 0x00ff_ffff) * 4
}

fn write_u24(destination: &mut [u8], value: u32) {
    destination[..3].copy_from_slice(&[(value >> 16) as u8, (value >> 8) as u8, value as u8]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rml_rtmp::chunk_io::ChunkDeserializer;

    fn serialize(message: &mut RtmpWireMessage) -> Vec<u8> {
        let mut output = Vec::new();
        while !message.is_complete() {
            let mut buffers: [&[u8]; MAX_CHUNKS_PER_IOV * 2] = [&[]; MAX_CHUNKS_PER_IOV * 2];
            let (count, bytes) = message.fill_buffers(4_096, &mut buffers);
            assert!(bytes > 0);
            output.extend(
                buffers[..count]
                    .iter()
                    .flat_map(|buffer| buffer.iter().copied()),
            );
            message.consume(bytes);
        }
        output
    }

    #[test]
    fn chunked_wire_message_round_trips_without_a_payload_copy() {
        let payload = Bytes::from((0..7_000).map(|value| value as u8).collect::<Vec<_>>());
        let expected = payload.clone();
        let mut message =
            RtmpWireMessage::new(9, RtmpTimestamp::new(77), 1, payload, 1_024).unwrap();
        let wire = serialize(&mut message);

        let mut deserializer = ChunkDeserializer::new();
        deserializer.set_max_chunk_size(1_024).unwrap();
        let decoded = deserializer.get_next_message(&wire).unwrap().unwrap();
        assert_eq!(decoded.type_id, 9);
        assert_eq!(decoded.timestamp.value, 77);
        assert_eq!(decoded.message_stream_id, 1);
        assert_eq!(decoded.data, expected);
    }

    #[test]
    fn extended_timestamp_and_partial_completion_preserve_wire_state() {
        let payload = Bytes::from_static(b"abcdef");
        let expected = payload.clone();
        let mut message =
            RtmpWireMessage::new(8, RtmpTimestamp::new(0x0100_0000), 3, payload, 3).unwrap();
        let mut wire = Vec::new();
        while !message.is_complete() {
            let mut buffers: [&[u8]; MAX_CHUNKS_PER_IOV * 2] = [&[]; MAX_CHUNKS_PER_IOV * 2];
            let (count, bytes) = message.fill_buffers(5, &mut buffers);
            assert!(bytes > 0);
            wire.extend(
                buffers[..count]
                    .iter()
                    .flat_map(|buffer| buffer.iter().copied()),
            );
            message.consume(bytes);
        }

        let mut deserializer = ChunkDeserializer::new();
        deserializer.set_max_chunk_size(3).unwrap();
        let decoded = deserializer.get_next_message(&wire).unwrap().unwrap();
        assert_eq!(decoded.timestamp.value, 0x0100_0000);
        assert_eq!(decoded.message_stream_id, 3);
        assert_eq!(decoded.data, expected);
    }
}
