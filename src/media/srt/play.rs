use std::os::raw::c_int;
use std::sync::Arc;

use tracing::{error, info, warn};

use super::SrtServer;
use super::socket::{SrtSocketGuard, try_acquire_srt_sender_permit};
use super::sys::{SRTSOCKET, srt_close, srt_send};
use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS;
use crate::media::ts_chunk_ring::TsChunkReader;

impl SrtServer {
    pub(super) async fn handle_play(&self, client_sock: SRTSOCKET, pipeline_id: &str) {
        if !self
            .engine
            .ingests
            .active
            .read()
            .await
            .contains_key(pipeline_id)
        {
            warn!("no active ingest for play: {}", pipeline_id);
            // SAFETY: no sender thread owns this live accepted socket because
            // the requested pipeline has no active ingest.
            unsafe {
                srt_close(client_sock);
            }
            return;
        }

        let ring_buf = self.engine.get_or_create_pipeline(pipeline_id).await;
        let shared_muxer = self
            .engine
            .get_or_create_ts_muxer_stage(pipeline_id, "play", ring_buf.clone())
            .await;

        let out_queue = Arc::new(crate::media::avio::MemoryQueue::new_with_capacity(
            self.engine.config.avio_capacity,
        ));

        let permit =
            match try_acquire_srt_sender_permit(self.engine.runtime.sender_semaphore.clone()) {
                Ok(permit) => permit,
                Err(_) => {
                    warn!(
                        "sender thread limit reached — rejecting play for {}",
                        pipeline_id
                    );
                    // SAFETY: the semaphore rejected ownership transfer to a
                    // sender thread, so this path still owns client_sock.
                    unsafe {
                        srt_close(client_sock);
                    }
                    return;
                }
            };
        let out_queue_send = out_queue.clone();
        let pid_log = pipeline_id.to_string();
        let out_queue_c = out_queue.clone();
        let play_sender_handle = std::thread::spawn(move || {
            let _permit = permit;
            let _client_sock_guard = SrtSocketGuard::new(client_sock);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = vec![0u8; 1316];
                loop {
                    let n = out_queue_send.read(&mut buf);
                    if n == 0 {
                        break;
                    }
                    // SAFETY: the sender thread exclusively owns client_sock;
                    // buf is live and n never exceeds the supplied slice.
                    let sent = unsafe { srt_send(client_sock, buf.as_ptr(), n as c_int) };
                    if sent < 0 {
                        break;
                    }
                }
            }));
            if result.is_err() {
                error!(
                    "[srt] Play sender thread panicked for pipeline: {}",
                    pid_log
                );
            } else {
                info!(
                    "[srt] Play subscriber disconnected for pipeline: {}",
                    pid_log
                );
            }
            out_queue_c.close();
        });
        self.engine.register_os_thread(play_sender_handle);

        let mut reader = TsChunkReader::new(format!("srt_play:{}", pipeline_id), &shared_muxer);
        let mut pull_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
        let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

        loop {
            let wake = reader.wait_for_data_or_cancelled().await;
            if out_queue.is_closed() {
                break;
            }
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
                if !ts_batch.is_empty() {
                    out_queue.write(&ts_batch).await;
                    ts_batch.clear();
                }
            }
            if out_queue.is_closed()
                || !self
                    .engine
                    .ingests
                    .active
                    .read()
                    .await
                    .contains_key(pipeline_id)
                || matches!(
                    wake,
                    crate::media::ts_chunk_ring::TsChunkWaitResult::Cancelled
                )
            {
                break;
            }
        }

        info!("Feed loop exited for pipeline={}", pipeline_id);
        out_queue.close();
    }
}
