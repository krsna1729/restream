//! Thin MPEG-TS chunk ring wrapper that gives TS consumers a cancelable view
//! over pre-muxed packets stored in the shared ring buffer.

use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::{Reader, RingBuffer};
use bytes::Bytes;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A thin wrapper around Arc<RingBuffer> where packets hold pre-muxed MPEG-TS chunks.
pub struct TsChunkRing {
    pub ring: Arc<RingBuffer>,
    pub cancel: CancellationToken,
    /// When this stage was created. `sweep_unused_stages` exempts stages
    /// younger than a grace window: a fabric consumer registers its
    /// liveness (`MediaEngine::fabric.srt.active_outputs`) asynchronously,
    /// after the stage already exists, so a reconcile tick landing in that
    /// gap must not treat "no reader yet" as "unused" and cancel it before
    /// the consumer ever gets a chance to attach.
    pub created_at: std::time::Instant,
}

impl TsChunkRing {
    pub fn new(capacity: usize, cancel: CancellationToken) -> Self {
        Self {
            ring: Arc::new(RingBuffer::new(capacity)),
            created_at: std::time::Instant::now(),
            cancel,
        }
    }

    pub fn push(&self, payload: Bytes, is_keyframe: bool) {
        let packet = MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload,
        };
        self.ring.push(packet);
    }

    pub fn push_batch<I>(&self, payloads: I) -> usize
    where
        I: IntoIterator<Item = (Bytes, bool)>,
    {
        let packets = payloads
            .into_iter()
            .map(|(payload, is_keyframe)| MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: 0,
                dts: 0,
                is_keyframe,
                format: PayloadFormat::Raw,
                payload,
            });
        self.ring.push_batch(packets)
    }
}

pub struct TsChunkReader {
    inner: Reader,
    cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsChunkWaitResult {
    Data,
    Cancelled,
}

impl TsChunkReader {
    pub fn new(name: String, ring: &TsChunkRing) -> Self {
        Self {
            inner: Reader::new(name, ring.ring.clone()),
            cancel: ring.cancel.clone(),
        }
    }

    pub fn new_with_keyframe_preroll(
        name: String,
        ring: &TsChunkRing,
        preroll_packets: usize,
    ) -> Self {
        Self {
            inner: Reader::new_with_keyframe_preroll(name, ring.ring.clone(), preroll_packets),
            cancel: ring.cancel.clone(),
        }
    }

    pub fn new_live(name: String, ring: &TsChunkRing) -> Self {
        Self {
            inner: Reader::new_live(name, ring.ring.clone()),
            cancel: ring.cancel.clone(),
        }
    }

    pub async fn wait_for_data_or_cancelled(&mut self) -> TsChunkWaitResult {
        tokio::select! {
            _ = self.cancel.cancelled() => TsChunkWaitResult::Cancelled,
            _ = self.inner.wait_for_data() => TsChunkWaitResult::Data,
        }
    }

    pub fn pull_burst(
        &mut self,
        output: &mut Vec<Arc<MediaPacket>>,
        max_packets: usize,
    ) -> Result<usize, &'static str> {
        self.inner.pull_burst(output, max_packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_readers_receive_same_chunks() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);

        let mut r1 = TsChunkReader::new("reader1".to_string(), &ts_ring);
        let mut r2 = TsChunkReader::new("reader2".to_string(), &ts_ring);

        // Push some chunks
        ts_ring.push(Bytes::from_static(b"chunk1"), true);
        ts_ring.push(Bytes::from_static(b"chunk2"), false);
        ts_ring.push_batch(vec![
            (Bytes::from_static(b"chunk3"), false),
            (Bytes::from_static(b"chunk4"), false),
        ]);

        let mut out1 = Vec::new();
        let mut out2 = Vec::new();

        let count1 = r1.pull_burst(&mut out1, 10).unwrap();
        let count2 = r2.pull_burst(&mut out2, 10).unwrap();

        assert_eq!(count1, 4);
        assert_eq!(count2, 4);

        let payloads1: Vec<&[u8]> = out1.iter().map(|p| &*p.payload).collect();
        let payloads2: Vec<&[u8]> = out2.iter().map(|p| &*p.payload).collect();

        assert_eq!(
            payloads1,
            vec![b"chunk1" as &[u8], b"chunk2", b"chunk3", b"chunk4"]
        );
        assert_eq!(
            payloads2,
            vec![b"chunk1" as &[u8], b"chunk2", b"chunk3", b"chunk4"]
        );
    }

    #[test]
    fn live_reader_starts_after_existing_chunks() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        ts_ring.push(Bytes::from_static(b"old"), true);

        let mut reader = TsChunkReader::new_live("live_reader".to_string(), &ts_ring);
        let mut out = Vec::new();
        assert_eq!(reader.pull_burst(&mut out, 10).unwrap(), 0);

        ts_ring.push(Bytes::from_static(b"new"), false);
        assert_eq!(reader.pull_burst(&mut out, 10).unwrap(), 1);
        assert_eq!(&*out[0].payload, b"new");
    }

    #[test]
    fn keyframe_preroll_reader_keeps_small_pre_keyframe_window() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        for i in 0..8 {
            ts_ring.push(Bytes::from(vec![i as u8]), i == 5);
        }

        let mut reader =
            TsChunkReader::new_with_keyframe_preroll("preroll".to_string(), &ts_ring, 2);
        let mut out = Vec::new();
        assert_eq!(reader.pull_burst(&mut out, 10).unwrap(), 5);
        let payloads: Vec<u8> = out.iter().map(|packet| packet.payload[0]).collect();
        assert_eq!(
            payloads,
            vec![3, 4, 5, 6, 7],
            "TS readers should retain a short pre-keyframe window for late joins"
        );
    }

    #[tokio::test]
    async fn wait_for_data_unblocks_when_ring_is_cancelled() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel.clone());
        let mut reader = TsChunkReader::new("reader".to_string(), &ts_ring);

        let wait_task = tokio::spawn(async move { reader.wait_for_data_or_cancelled().await });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();

        assert_eq!(wait_task.await.unwrap(), TsChunkWaitResult::Cancelled);
    }

    // The `select!` in `wait_for_data_or_cancelled` only ever had its
    // `Cancelled` arm exercised in this file; nothing asserted that a chunk
    // arriving first resolves the `Data` arm instead.
    #[tokio::test]
    async fn wait_for_data_unblocks_with_data_when_chunk_arrives_before_cancel() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel.clone());
        let mut reader = TsChunkReader::new("reader".to_string(), &ts_ring);

        let wait_task = tokio::spawn(async move {
            let result = reader.wait_for_data_or_cancelled().await;
            (result, reader)
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        ts_ring.push(Bytes::from_static(b"chunk"), true);

        let (result, mut reader) = wait_task.await.unwrap();
        assert_eq!(result, TsChunkWaitResult::Data);

        let mut out = Vec::new();
        assert_eq!(reader.pull_burst(&mut out, 10).unwrap(), 1);
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn push_batch_returns_the_number_of_packets_pushed() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);

        assert_eq!(ts_ring.push_batch(std::iter::empty()), 0);
        assert_eq!(
            ts_ring.push_batch(vec![
                (Bytes::from_static(b"a"), false),
                (Bytes::from_static(b"b"), false),
                (Bytes::from_static(b"c"), true),
            ]),
            3
        );
    }

    #[test]
    fn pull_burst_with_zero_max_packets_pulls_nothing() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        ts_ring.push(Bytes::from_static(b"chunk"), true);

        let mut reader = TsChunkReader::new("reader".to_string(), &ts_ring);
        let mut out = Vec::new();
        assert_eq!(reader.pull_burst(&mut out, 0).unwrap(), 0);
        assert!(out.is_empty());

        // The chunk must still be available on a subsequent pull rather than
        // having been silently consumed by the zero-limit call.
        assert_eq!(reader.pull_burst(&mut out, 10).unwrap(), 1);
    }
}
