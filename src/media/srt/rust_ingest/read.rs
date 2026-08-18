use std::sync::Arc;

use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use super::super::SrtServer;
use super::types::{ConnectionId, WorkerCommand};
use crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS;
use crate::media::ts_chunk_ring::{TsChunkReader, TsChunkWaitResult};
use crate::media::{MEDIA_TS_BATCH_TARGET_BYTES, SRT_TS_PAYLOAD_BYTES};

pub(super) fn spawn(
    server: Arc<SrtServer>,
    id: ConnectionId,
    pipeline_id: String,
    commands: Sender<WorkerCommand>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        run(server, id, pipeline_id, commands, task_cancel).await;
    });
    cancel
}

async fn run(
    server: Arc<SrtServer>,
    id: ConnectionId,
    pipeline_id: String,
    commands: Sender<WorkerCommand>,
    cancel: CancellationToken,
) {
    let ring_buffer = server.engine.get_or_create_pipeline(&pipeline_id).await;
    let shared_muxer = server
        .engine
        .get_or_create_ts_muxer_stage(&pipeline_id, "rust-play", ring_buffer)
        .await;
    let mut reader = TsChunkReader::new(format!("rust_srt_play:{pipeline_id}"), &shared_muxer);
    let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut ts_batch = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

    loop {
        let Some(wake) = wait_for_data_or_session_cancelled(&mut reader, &cancel).await else {
            tracing::debug!(?id, %pipeline_id, "Rust SRT read cancelled while waiting for media");
            break;
        };

        loop {
            pull_packets.clear();
            match reader.pull_burst(&mut pull_packets, MEDIA_PULL_BURST_PACKETS) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            for packet in &pull_packets {
                if !packet.payload.is_empty() {
                    ts_batch.extend_from_slice(&packet.payload);
                }
            }
            if ts_batch.is_empty() {
                continue;
            }
            if !send_ts_batch(&cancel, &commands, id, &ts_batch).await {
                tracing::debug!(?id, %pipeline_id, "Rust SRT read command channel closed");
                return;
            }
            ts_batch.clear();
        }

        let active = server
            .engine
            .ingests
            .active
            .read()
            .await
            .contains_key(&pipeline_id);
        if !active || matches!(wake, TsChunkWaitResult::Cancelled) {
            tracing::debug!(?id, %pipeline_id, active, ?wake, "Rust SRT read stopping");
            break;
        }
    }

    if !cancel.is_cancelled() {
        let _ = commands
            .send(WorkerCommand::Close {
                id,
                reason: "Rust SRT read session ended".to_string(),
            })
            .await;
    }
}

async fn wait_for_data_or_session_cancelled(
    reader: &mut TsChunkReader,
    cancel: &CancellationToken,
) -> Option<TsChunkWaitResult> {
    tokio::select! {
        _ = cancel.cancelled() => None,
        wake = reader.wait_for_data_or_cancelled() => Some(wake),
    }
}

async fn send_ts_batch(
    cancel: &CancellationToken,
    commands: &Sender<WorkerCommand>,
    id: ConnectionId,
    payload: &[u8],
) -> bool {
    for chunk in payload.chunks(SRT_TS_PAYLOAD_BYTES) {
        let sent = tokio::select! {
            _ = cancel.cancelled() => false,
            result = commands.send(WorkerCommand::Send {
                id,
                payload: chunk.to_vec(),
            }) => result.is_ok(),
        };
        if !sent {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_batches_are_split_at_srt_payload_limit() {
        let payload = vec![0u8; SRT_TS_PAYLOAD_BYTES * 2 + 17];
        let id = ConnectionId {
            worker: 0,
            serial: 1,
        };
        let cancel = CancellationToken::new();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(3);

        assert!(send_ts_batch(&cancel, &sender, id, &payload).await);
        drop(sender);

        let mut chunks = Vec::new();
        while let Some(WorkerCommand::Send {
            id: message_id,
            payload,
        }) = receiver.recv().await
        {
            assert_eq!(message_id, id);
            chunks.push(payload);
        }

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), SRT_TS_PAYLOAD_BYTES);
        assert_eq!(chunks[1].len(), SRT_TS_PAYLOAD_BYTES);
        assert_eq!(chunks[2].len(), 17);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= SRT_TS_PAYLOAD_BYTES)
        );
    }

    #[tokio::test]
    async fn session_cancellation_wakes_idle_reader() {
        let ring = crate::media::ts_chunk_ring::TsChunkRing::new(16, CancellationToken::new());
        let mut reader = TsChunkReader::new("read-test".to_string(), &ring);
        let cancel = CancellationToken::new();
        let waiter = tokio::spawn({
            let cancel = cancel.clone();
            async move { wait_for_data_or_session_cancelled(&mut reader, &cancel).await }
        });

        tokio::task::yield_now().await;
        cancel.cancel();

        assert_eq!(waiter.await.expect("reader waiter completes"), None);
    }
}
