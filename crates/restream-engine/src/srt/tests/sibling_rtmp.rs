//! Independently owned RTMP publishers/readers sharing the faulted SRT engine.
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::engine::MediaEngine;
use crate::security::IngestSecurityService;
use restream_domain::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;

pub(super) struct RtmpVictims {
    pub(super) engine: Arc<MediaEngine>,
    pub(super) listener: JoinHandle<()>,
    pub(super) received: Vec<Arc<AtomicU64>>,
    cancel: CancellationToken,
    children: Vec<Child>,
    readers: Vec<JoinHandle<()>>,
}

impl RtmpVictims {
    pub(super) async fn start() -> Self {
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let (ready, started) = tokio::sync::oneshot::channel();
        let listener = tokio::spawn(crate::rtmp::start_rtmp_server_on_with_shutdown(
            super::sibling_fault_gate::auth(),
            Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG)),
            engine.clone(),
            0,
            cancel.clone(),
            Some(ready),
        ));
        let addr = tokio::time::timeout(Duration::from_secs(10), started)
            .await
            .expect("RTMP listener startup bounded")
            .expect("RTMP startup reports")
            .expect("RTMP listener binds");
        let mut victims = Self {
            engine,
            listener,
            received: Vec::new(),
            cancel,
            children: Vec::new(),
            readers: Vec::new(),
        };
        let fixture = crate::test_fixtures::canonical_h264_ts_fixture()
            .expect("checked-in publisher fixture");
        for key in ["selected", "standby", "other"] {
            let child = ffmpeg()
                .args(["-re", "-stream_loop", "-1", "-i"])
                .arg(&fixture)
                .args(["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy", "-f", "flv"])
                .arg(format!("rtmp://127.0.0.1:{}/live/{key}", addr.port()))
                .stdout(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("real RTMP publisher");
            victims.children.push(child);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if victims.engine.ingests.sessions.read().await.len() == 3 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "RTMP victim inputs did not register"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for key in ["selected", "selected", "other"] {
            let mut child = ffmpeg()
                .args([
                    "-i",
                    &format!("rtmp://127.0.0.1:{}/live/{key}", addr.port()),
                ])
                .args([
                    "-map",
                    "0:v:0",
                    "-c",
                    "copy",
                    "-f",
                    "mpegts",
                    "-flush_packets",
                    "1",
                    "pipe:1",
                ])
                .stdout(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("real RTMP reader");
            let mut pipe = child.stdout.take().expect("reader output pipe");
            let received = Arc::new(AtomicU64::new(0));
            let counter = received.clone();
            victims.readers.push(tokio::spawn(async move {
                let mut buffer = [0; 16_384];
                loop {
                    let count = pipe.read(&mut buffer).await.expect("read received media");
                    if count == 0 {
                        break;
                    }
                    counter.fetch_add(count as u64, Ordering::Release);
                }
            }));
            victims.received.push(received);
            victims.children.push(child);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while !victims
            .received
            .iter()
            .all(|bytes| bytes.load(Ordering::Acquire) > 10_000)
        {
            for child in &mut victims.children {
                assert!(
                    child.try_wait().expect("victim child state").is_none(),
                    "RTMP victim exited before delivering"
                );
            }
            assert!(
                Instant::now() < deadline,
                "RTMP real consumers never delivered"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        victims
    }

    pub(super) async fn finish(mut self) {
        for child in &mut self.children {
            assert!(
                child.try_wait().expect("victim child state").is_none(),
                "healthy RTMP peer exited through Owner fault"
            );
        }
        for child in &mut self.children {
            crate::child_process::reap(child, Duration::ZERO)
                .await
                .expect("victim child reaped");
        }
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(10), &mut self.listener)
            .await
            .expect("RTMP listener stops")
            .expect("RTMP listener did not panic");
    }
}

impl Drop for RtmpVictims {
    fn drop(&mut self) {
        self.cancel.cancel();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

fn ffmpeg() -> Command {
    let mut command = Command::new(crate::ffmpeg_binary::ffmpeg_bin_path());
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    command
}
