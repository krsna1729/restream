#![cfg(target_os = "linux")]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::config::EngineConfig;
use crate::egress::command::{EgressCommand, FeedId, OutputId};
use crate::egress::journal::{FeedEpoch, RingFeed};
use crate::egress::manager::ManagerCommandOutcome;
use crate::egress::runtime::FabricWorkerError;
use crate::engine::MediaEngine;
use crate::engine_hls_egress_fabric::HlsPutFabricEnsureError;
use crate::engine_rtmp_egress_fabric::RtmpFabricEnsureError;
use crate::hls::HlsStore;

const PROGRESS_BOUND: Duration = Duration::from_secs(1);
const WATCHDOG_BOUND: Duration = Duration::from_secs(5);
static NEXT_TRUST_FILE: AtomicU64 = AtomicU64::new(0);

struct TrustFile {
    dir: PathBuf,
    path: PathBuf,
    pem: Vec<u8>,
    cleaned: AtomicBool,
}

impl TrustFile {
    async fn new() -> Arc<Self> {
        tokio::task::spawn_blocking(|| {
            let sequence = NEXT_TRUST_FILE.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "restream-fabric-construction-{}-{sequence}",
                std::process::id(),
            ));
            fs::create_dir(&dir).unwrap();
            let path = dir.join("extra-ca.pem");
            let key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let pem = params.self_signed(&key).unwrap().pem().into_bytes();
            fs::write(&path, &pem).unwrap();
            Arc::new(Self {
                dir,
                path,
                pem,
                cleaned: AtomicBool::new(false),
            })
        })
        .await
        .unwrap()
    }

    fn engine(&self) -> Arc<MediaEngine> {
        Arc::new(MediaEngine::new_with_config(Arc::new(EngineConfig {
            rtmps_extra_trust_roots_pem_path: Some(self.path.to_str().unwrap().to_owned()),
            ..EngineConfig::default()
        })))
    }

    async fn restore_regular_file(self: &Arc<Self>) {
        let trust = self.clone();
        tokio::task::spawn_blocking(move || {
            // The first candidate already owns the unlinked FIFO's read fd.
            // A second candidate reads a separate regular file at the same path.
            fs::remove_file(&trust.path).unwrap();
            fs::write(&trust.path, &trust.pem).unwrap();
        })
        .await
        .unwrap();
    }

    async fn cleanup(self: Arc<Self>) {
        tokio::task::spawn_blocking(move || {
            if !self.cleaned.swap(true, Ordering::AcqRel) {
                fs::remove_dir_all(&self.dir).unwrap();
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for TrustFile {
    fn drop(&mut self) {
        if !self.cleaned.swap(true, Ordering::AcqRel) {
            // Unwind cleanup must not perform filesystem I/O on the test runtime.
            let dir = self.dir.clone();
            std::thread::spawn(move || {
                let _ = std::panic::catch_unwind(move || {
                    let _ = fs::remove_dir_all(dir);
                });
            });
        }
    }
}

struct HeldTrustRead {
    release: mpsc::Sender<()>,
    ready: Option<tokio::sync::oneshot::Receiver<()>>,
    watchdog_released: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl HeldTrustRead {
    async fn install(trust: &Arc<TrustFile>) -> Self {
        let trust = trust.clone();
        tokio::task::spawn_blocking(move || {
            fs::remove_file(&trust.path).unwrap();
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                &trust.path,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
            let (release, wait_release) = mpsc::channel();
            let (ready, wait_ready) = tokio::sync::oneshot::channel();
            let watchdog_released = Arc::new(AtomicBool::new(false));
            let watchdog = watchdog_released.clone();
            let worker = std::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let deadline = Instant::now() + WATCHDOG_BOUND;
                    let mut writer = loop {
                        // This succeeds only after the production PEM reader opens
                        // the FIFO. Keeping this fd open without bytes holds its read.
                        match OpenOptions::new()
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&trust.path)
                        {
                            Ok(writer) => break writer,
                            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                                if Instant::now() >= deadline {
                                    return Err(
                                        "production trust-root reader never opened FIFO".into()
                                    );
                                }
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(error) => return Err(error.to_string()),
                        }
                    };
                    let _ = ready.send(());
                    if matches!(
                        wait_release.recv_timeout(WATCHDOG_BOUND),
                        Err(mpsc::RecvTimeoutError::Timeout),
                    ) {
                        // Set before releasing: a blocked current-thread runtime
                        // resumes after the watchdog and fails, rather than hangs.
                        watchdog.store(true, Ordering::Release);
                    }
                    writer
                        .write_all(&trust.pem)
                        .map_err(|error| error.to_string())?;
                    drop(writer); // EOF completes the real PEM file iterator.
                    Ok(())
                }))
                .unwrap_or_else(|_| Err("trust-root watchdog panicked".into()))
            });
            Self {
                release,
                ready: Some(wait_ready),
                watchdog_released,
                worker: Some(worker),
            }
        })
        .await
        .unwrap()
    }

    async fn wait_until_held(&mut self) {
        tokio::time::timeout(WATCHDOG_BOUND + PROGRESS_BOUND, self.ready.take().unwrap())
            .await
            .expect("FIFO reader did not rendezvous")
            .expect("FIFO writer failed before rendezvous");
        self.assert_held();
    }

    fn assert_held(&self) {
        assert!(
            !self.watchdog_released.load(Ordering::Acquire),
            "Tokio only progressed after the trust-root watchdog released the read",
        );
    }

    fn release(&self) {
        let _ = self.release.send(());
    }

    async fn finish(mut self) {
        self.release();
        let worker = self.worker.take().unwrap();
        tokio::task::spawn_blocking(move || worker.join().unwrap().unwrap())
            .await
            .unwrap();
    }
}

impl Drop for HeldTrustRead {
    fn drop(&mut self) {
        // Also release on assertion failure; no join or filesystem work here.
        self.release();
    }
}

#[derive(Clone, Copy)]
enum Protocol {
    Rtmp,
    Hls,
}

enum Feed {
    Rtmp(RingFeed),
    Hls(Arc<HlsStore>),
}

#[derive(Debug, PartialEq, Eq)]
enum RetainError {
    Rtmp(RtmpFabricEnsureError),
    Hls(HlsPutFabricEnsureError),
}

impl Protocol {
    fn feed(self) -> Feed {
        match self {
            Self::Rtmp => Feed::Rtmp(RingFeed::new(
                Arc::new(restream_sync::RingBuffer::new(8)),
                Arc::new(FeedEpoch::new()),
            )),
            Self::Hls => Feed::Hls(Arc::new(HlsStore::new())),
        }
    }

    async fn retain(
        self,
        engine: &MediaEngine,
        id: FeedId,
        feed: &Feed,
    ) -> Result<bool, RetainError> {
        match feed {
            Feed::Rtmp(feed) => engine
                .retain_rtmp_fabric_runtime(id, feed)
                .await
                .map_err(RetainError::Rtmp),
            Feed::Hls(store) => engine
                .retain_hls_put_fabric_runtime(id, store)
                .await
                .map_err(RetainError::Hls),
        }
    }

    async fn remove(self, engine: &MediaEngine, id: &FeedId) -> ManagerCommandOutcome {
        let command = EgressCommand::Remove(OutputId::new("already-absent"));
        match self {
            Self::Rtmp => engine
                .dispatch_rtmp_fabric_command(id, command)
                .await
                .unwrap(),
            Self::Hls => engine
                .dispatch_hls_put_fabric_command(id, command)
                .await
                .unwrap(),
        }
    }

    async fn release(self, engine: &MediaEngine, id: &FeedId) -> bool {
        match self {
            Self::Rtmp => engine.release_rtmp_fabric_runtime(id).await,
            Self::Hls => engine.release_hls_put_fabric_runtime(id).await,
        }
    }

    async fn shutdown_all(self, engine: &MediaEngine) -> usize {
        match self {
            Self::Rtmp => engine.shutdown_all_rtmp_fabric_runtimes().await,
            Self::Hls => engine.shutdown_all_hls_put_fabric_runtimes().await,
        }
    }

    fn closed_error(self) -> RetainError {
        match self {
            Self::Rtmp => {
                RetainError::Rtmp(RtmpFabricEnsureError::Worker(FabricWorkerError::Closed))
            }
            Self::Hls => {
                RetainError::Hls(HlsPutFabricEnsureError::Worker(FabricWorkerError::Closed))
            }
        }
    }
}

async fn held_construction_does_not_block_sibling(protocol: Protocol) {
    let trust = TrustFile::new().await;
    let engine = trust.engine();
    let healthy_id = FeedId::new("healthy-before-held-construction");
    assert_eq!(
        protocol
            .retain(&engine, healthy_id.clone(), &protocol.feed())
            .await,
        Ok(true)
    );
    let mut held = HeldTrustRead::install(&trust).await;
    let late_engine = engine.clone();
    let late_id = FeedId::new("held-construction");
    let late = tokio::spawn(async move {
        protocol
            .retain(&late_engine, late_id, &protocol.feed())
            .await
    });
    held.wait_until_held().await;
    let progress = tokio::time::timeout(PROGRESS_BOUND, async {
        tokio::join!(
            protocol.remove(&engine, &healthy_id),
            tokio::time::sleep(Duration::from_millis(20)),
        )
    })
    .await
    .expect("a held constructor blocked same-protocol dispatch or the Tokio timer");
    assert_eq!(progress.0, ManagerCommandOutcome::AlreadyRemoved);
    held.assert_held();
    assert!(
        !late.is_finished(),
        "construction completed before FIFO release"
    );
    held.release();
    assert_eq!(late.await.unwrap(), Ok(true));
    held.finish().await;
    assert_eq!(protocol.shutdown_all(&engine).await, 2);
    trust.cleanup().await;
}

async fn shutdown_fences_held_construction(protocol: Protocol) {
    let trust = TrustFile::new().await;
    let engine = trust.engine();
    let mut held = HeldTrustRead::install(&trust).await;
    let id = FeedId::new("construction-crossing-shutdown");
    let late_engine = engine.clone();
    let late_id = id.clone();
    let late = tokio::spawn(async move {
        protocol
            .retain(&late_engine, late_id, &protocol.feed())
            .await
    });
    held.wait_until_held().await;
    assert_eq!(
        tokio::time::timeout(PROGRESS_BOUND, protocol.shutdown_all(&engine))
            .await
            .expect("shutdown waited for a constructor holding the root registry"),
        0,
    );
    held.assert_held();
    assert!(!late.is_finished());
    held.release();
    assert_eq!(late.await.unwrap(), Err(protocol.closed_error()));
    held.finish().await;
    assert!(
        !protocol.release(&engine, &id).await,
        "late constructor resurrected a refcount"
    );
    assert_eq!(
        protocol.shutdown_all(&engine).await,
        0,
        "late constructor resurrected a runtime"
    );
    trust.cleanup().await;
}

async fn competing_constructors_retain_one_runtime(protocol: Protocol) {
    let trust = TrustFile::new().await;
    let engine = trust.engine();
    let mut held = HeldTrustRead::install(&trust).await;
    let id = FeedId::new("competing-construction");
    let first_engine = engine.clone();
    let first_id = id.clone();
    let first = tokio::spawn(async move {
        protocol
            .retain(&first_engine, first_id, &protocol.feed())
            .await
    });
    held.wait_until_held().await;
    trust.restore_regular_file().await;
    // The second creator must win while the first still owns its blocked
    // FIFO descriptor. Both candidates therefore crossed the initial lookup.
    assert_eq!(
        tokio::time::timeout(
            PROGRESS_BOUND,
            protocol.retain(&engine, id.clone(), &protocol.feed())
        )
        .await
        .expect("same-feed creation waited behind an unfinished candidate"),
        Ok(true),
    );
    held.assert_held();
    assert!(!first.is_finished());
    held.release();
    assert_eq!(first.await.unwrap(), Ok(false));
    held.finish().await;
    assert!(
        !protocol.release(&engine, &id).await,
        "first release dropped both references"
    );
    assert_eq!(
        protocol.remove(&engine, &id).await,
        ManagerCommandOutcome::AlreadyRemoved
    );
    assert!(
        protocol.release(&engine, &id).await,
        "final release failed to remove the runtime"
    );
    assert!(!protocol.release(&engine, &id).await);
    assert_eq!(protocol.shutdown_all(&engine).await, 0);
    trust.cleanup().await;
}

#[tokio::test(flavor = "current_thread")]
async fn rtmp_held_trust_read_isolates_sibling_dispatch_and_tokio_timer() {
    held_construction_does_not_block_sibling(Protocol::Rtmp).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hls_held_trust_read_isolates_sibling_dispatch_and_tokio_timer() {
    held_construction_does_not_block_sibling(Protocol::Hls).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rtmp_shutdown_fences_pending_constructor() {
    shutdown_fences_held_construction(Protocol::Rtmp).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hls_shutdown_fences_pending_constructor() {
    shutdown_fences_held_construction(Protocol::Hls).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rtmp_competing_constructors_account_each_reference_once() {
    competing_constructors_retain_one_runtime(Protocol::Rtmp).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hls_competing_constructors_account_each_reference_once() {
    competing_constructors_retain_one_runtime(Protocol::Hls).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_busy_feed_cannot_block_periodic_recovery_of_a_quiet_sibling() {
    use crate::egress::backends::sink_shard::INJECTED_SHARD_PANIC_OUTPUT;
    use crate::test_support::output_spec;

    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    let engine = MediaEngine::new_with_config(Arc::new(EngineConfig {
        egress_fabric: crate::config::EgressFabricConfig {
            shards: 1,
            ..Default::default()
        },
        ..Default::default()
    }));
    let busy_id = FeedId::new("busy-maintenance-feed");
    let quiet_id = FeedId::new("quiet-panicked-feed");
    let feed = RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(8)),
        Arc::new(FeedEpoch::new()),
    );
    engine
        .retain_sink_fabric_runtime(busy_id.clone(), &feed)
        .await
        .unwrap();
    engine
        .retain_sink_fabric_runtime(quiet_id.clone(), &feed)
        .await
        .unwrap();
    let ended = Arc::new(AtomicBool::new(false));
    let mut spec = output_spec(INJECTED_SHARD_PANIC_OUTPUT, &quiet_id);
    spec.progress.terminated_unexpectedly = Some(ended.clone());
    engine
        .dispatch_sink_fabric_command(&quiet_id, EgressCommand::Add(spec))
        .await
        .unwrap();
    tokio::time::timeout(PROGRESS_BOUND, async {
        while !engine
            .sink_fabric_runtime_snapshots(&quiet_id)
            .await
            .unwrap()[0]
            .panicked
        {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the quiet feed's shard must fault before maintenance");

    let busy = engine.fabric.sink.lock().await.runtimes[&busy_id].clone();
    let (entered, wait_entered) = tokio::sync::oneshot::channel();
    let (release, wait_release) = mpsc::channel();
    let release = Release(release);
    let held = tokio::spawn(async move {
        busy.with(move |_| {
            let _ = entered.send(());
            wait_release.recv_timeout(WATCHDOG_BOUND)
        })
        .await
    });
    wait_entered.await.unwrap();
    tokio::time::timeout(PROGRESS_BOUND, engine.resize_sink_fabrics())
        .await
        .expect("periodic sizing must not await a busy feed");
    tokio::time::timeout(PROGRESS_BOUND, async {
        loop {
            let healthy = engine
                .sink_fabric_runtime_snapshots(&quiet_id)
                .await
                .unwrap()
                .iter()
                .all(|snapshot| !snapshot.panicked && !snapshot.stopped);
            if ended.load(Ordering::Acquire) && healthy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the quiet sibling recovers while the other feed remains held");
    assert!(
        !held.is_finished(),
        "the watchdog must not release the busy feed first"
    );
    drop(release);
    held.await.unwrap().unwrap().unwrap();
    assert_eq!(engine.shutdown_all_sink_fabric_runtimes().await, 2);
}
