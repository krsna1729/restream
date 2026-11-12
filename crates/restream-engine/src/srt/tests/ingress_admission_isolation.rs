//! Admission runs off the shared SRT session loop: a stalled pipeline lookup
//! for one tenant must not stall another tenant's connect, an admission whose
//! peer left must not leak its registration, and a listener exit must finish
//! what late admissions registered. Production `SrtServer` over a real Owner
//! and real srt-rs callers; only the pipeline lookup is gated.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use srt_transport::{CallerConfig, SocketOwnership};

use crate::engine::MediaEngine;

use super::super::ingress_test_support::*;
use super::super::tokio_ingress::{AUTH_DEADLINE, MAX_ADMISSIONS_PER_KEY};

fn publish_id(key: &str) -> String {
    format!("#!::r={key},m=publish")
}

/// Hold a publisher connection open, sending nothing, until `stop` or the
/// listener disconnects it.
fn hold(
    remote: SocketAddr,
    key: &str,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<CallerReport> {
    hold_attach(direct(remote, &publish_id(key), None), stop)
}

fn hold_attach(attach: Attach, stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<CallerReport> {
    tokio::task::spawn_blocking(move || {
        run_caller(attach, Duration::from_secs(20), move |ctx| {
            ctx.report.disconnected || stop.load(Ordering::Acquire)
        })
    })
}

/// Publish the fixture until it is sent or `stop` is set.
fn publish(attach: Attach, stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<CallerReport> {
    tokio::task::spawn_blocking(move || {
        let mut step = publish_step(ts_chunks(400));
        run_caller(attach, Duration::from_secs(20), move |ctx| {
            step(ctx) || stop.load(Ordering::Acquire)
        })
    })
}

async fn active(engine: &MediaEngine, pipeline_id: &str) -> bool {
    engine.ingests.active.read().await.contains_key(pipeline_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_pipeline_lookup_does_not_stall_another_tenants_publisher() {
    let auth = GatedAuth::new(&["stall-a", "stall-b"], &["stall-a"]);
    let server = TestServer::start_with_auth(
        auth.clone(),
        vec![plain("stall-a"), plain("stall-b")],
        Arc::new(MediaEngine::new()),
    )
    .await;
    let remote = server.remote;
    let stop = Arc::new(AtomicBool::new(false));
    let a = hold(remote, "stall-a", stop.clone());
    server
        .wait_until("A's lookup started", Duration::from_secs(5), || async {
            auth.entered() == 1
        })
        .await;

    let b = publish(direct(remote, &publish_id("stall-b"), None), stop.clone());
    server
        .wait_until(
            "B's media reaches its ring while A's lookup is stalled",
            Duration::from_secs(4),
            || async { server.ring_progress("pipeline-stall-b").await > 0 },
        )
        .await;
    assert_eq!(auth.completed(), 0, "A was still pending while B published");

    auth.open();
    server
        .wait_until(
            "A registers once its lookup answers",
            Duration::from_secs(4),
            || async { server.ingest_active("pipeline-stall-a").await },
        )
        .await;
    stop.store(true, Ordering::Release);
    assert!(a.await.unwrap().connected);
    assert!(b.await.unwrap().connected);
    server.stop().await;
}

/// The encoder gives up on A while A's lookup is stalled and reconnects as B
/// on the same key; A's lookup answers first. A must register nothing, so B
/// is not refused as a duplicate of its own abandoned attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_after_a_stalled_lookup_is_not_refused_as_a_duplicate() {
    let auth = GatedAuth::new(&["rejoin"], &["rejoin"]);
    let server = TestServer::start_with_auth(
        auth.clone(),
        vec![plain("rejoin")],
        Arc::new(MediaEngine::new()),
    )
    .await;
    let remote = server.remote;

    // A connects and leaves while its lookup is stalled.
    let leaver = auth.clone();
    let report = blocking(move || {
        run_caller(
            direct(remote, &publish_id("rejoin"), None),
            Duration::from_secs(10),
            // The listener can admit before the caller sees its handshake
            // answer; leave only once both sides are connected.
            move |ctx| ctx.report.connected && leaver.entered() == 1,
        )
    })
    .await;
    assert!(report.connected, "{report:?}");
    let owner = server.engine.listener_stats_handle();
    server
        .wait_until("the Owner retires A", Duration::from_secs(5), || async {
            owner.ingress_owner.snapshot().peers == 0
        })
        .await;
    // B reconnects on the key, from its own address so its registration is
    // identifiable. One Owner's events reach Tokio in order, so B's lookup
    // starting proves the loop consumed A's Disconnected.
    let stop = Arc::new(AtomicBool::new(false));
    let b = hold_attach(from_ip(remote, 30, "rejoin"), stop.clone());
    server
        .wait_until("B's lookup started", Duration::from_secs(5), || async {
            auth.entered() == 2
        })
        .await;

    // A's lookup answers first, then B's.
    let attempts = server.engine.ingests.next_attempt_id.load(Ordering::SeqCst);
    auth.release(1);
    server
        .wait_until("A's lookup answered", Duration::from_secs(3), || async {
            auth.completed() == 1
        })
        .await;
    auth.release(1);
    server
        .wait_until("B registers on the key", Duration::from_secs(5), || async {
            let active = server.engine.ingests.active.read().await;
            active.get("pipeline-rejoin").is_some_and(|ingest| {
                restream_sync::lock::read(&ingest.metadata)
                    .remote_addr
                    .as_deref()
                    .is_some_and(|addr| addr.starts_with("127.0.0.30:"))
            })
        })
        .await;
    assert_eq!(
        server.engine.ingests.next_attempt_id.load(Ordering::SeqCst),
        attempts + 1,
        "only B registered; the abandoned admission registered nothing"
    );
    stop.store(true, Ordering::Release);
    let b = b.await.unwrap();
    assert!(b.connected && !b.disconnected, "B was never refused: {b:?}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lookup_past_the_deadline_rejects_the_peer_and_the_loop_keeps_serving() {
    let auth = GatedAuth::new(&["late-a", "late-b"], &["late-a"]);
    let server = TestServer::start_with_auth(
        auth.clone(),
        vec![plain("late-a"), plain("late-b")],
        Arc::new(MediaEngine::new()),
    )
    .await;
    let remote = server.remote;
    let report = blocking(move || {
        run_caller(
            direct(remote, &publish_id("late-a"), None),
            AUTH_DEADLINE + Duration::from_secs(4),
            |ctx| ctx.report.disconnected,
        )
    })
    .await;
    let (Some(up), Some(down)) = (report.connected_at, report.disconnected_at) else {
        panic!("the stalled admission never disconnected its peer: {report:?}");
    };
    let held = down.duration_since(up);
    assert!(
        held + Duration::from_millis(500) >= AUTH_DEADLINE
            && held < AUTH_DEADLINE + Duration::from_secs(3),
        "disconnected by the lookup deadline, not idle timeout: {held:?}"
    );
    assert_eq!(auth.entered(), 1);
    assert_eq!(auth.completed(), 0, "the timed-out lookup was dropped");
    assert!(!server.ingest_active("pipeline-late-a").await);

    let stop = Arc::new(AtomicBool::new(false));
    let b = hold(remote, "late-b", stop.clone());
    server
        .wait_until(
            "B registers after the timeout",
            Duration::from_secs(5),
            || async { server.ingest_active("pipeline-late-b").await },
        )
        .await;
    stop.store(true, Ordering::Release);
    assert!(b.await.unwrap().connected);
    server.stop().await;
}

/// The per-IP cap equals the Owner's own per-IP peer limit, so it only bites
/// on admissions whose peer already left: a client that connects and leaves
/// while lookups stall cannot pile up tasks beyond what it could hold open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admissions_per_ip_are_bounded_without_affecting_others() {
    const PER_IP: usize = 4;
    let keys: Vec<String> = (0..=PER_IP).map(|index| format!("bound-{index}")).collect();
    let gated: Vec<&str> = keys.iter().map(String::as_str).collect();
    let mut all = gated.clone();
    all.push("bound-other");
    let auth = GatedAuth::new(&all, &gated);
    let mut config = crate::config::EngineConfig::from_env();
    config.srt_max_peers_per_ip = PER_IP;
    let server = TestServer::start_with_auth(
        auth.clone(),
        all.iter().map(|key| plain(key)).collect(),
        Arc::new(MediaEngine::new_with_config(Arc::new(config))),
    )
    .await;
    let remote = server.remote;

    // PER_IP peers from one address connect, stall in the lookup and leave.
    let leave = Arc::new(AtomicBool::new(false));
    let leavers: Vec<_> = gated[..PER_IP]
        .iter()
        .map(|key| hold(remote, key, leave.clone()))
        .collect();
    server
        .wait_until("the per-IP cap fills", Duration::from_secs(5), || async {
            auth.entered() == PER_IP
        })
        .await;
    leave.store(true, Ordering::Release);
    for leaver in leavers {
        assert!(leaver.await.unwrap().connected);
    }
    let owner = server.engine.listener_stats_handle();
    server
        .wait_until("the Owner retires them", Duration::from_secs(3), || async {
            owner.ingress_owner.snapshot().peers == 0
        })
        .await;

    // The Owner admits one more from that address, but its abandoned
    // admissions fill the cap: refused without a lookup, while a caller from
    // another address is admitted.
    let extra = publish_id(gated[PER_IP]);
    let extra = blocking(move || {
        run_caller(
            direct(remote, &extra, None),
            Duration::from_secs(4),
            |ctx| ctx.report.disconnected,
        )
    });
    let stop = Arc::new(AtomicBool::new(false));
    let other = hold_attach(from_ip(remote, 2, "bound-other"), stop.clone());
    let extra = extra.await;
    let (Some(up), Some(down)) = (extra.connected_at, extra.disconnected_at) else {
        panic!("the over-cap peer was not refused: {extra:?}");
    };
    assert!(
        down.duration_since(up) < Duration::from_secs(2),
        "refused at once, not by the lookup deadline: {extra:?}"
    );
    server
        .wait_until(
            "another address is admitted",
            Duration::from_secs(4),
            || async { server.ingest_active("pipeline-bound-other").await },
        )
        .await;
    assert_eq!(
        auth.entered(),
        PER_IP,
        "the over-cap peer never reached the lookup"
    );

    // Once the stalled lookups answer, the abandoned admissions register
    // nothing and free the address's slots.
    auth.open();
    server
        .wait_until(
            "the stalled lookups answer",
            Duration::from_secs(3),
            || async { auth.completed() == PER_IP },
        )
        .await;
    let again = hold(remote, gated[PER_IP], stop.clone());
    server
        .wait_until(
            "the address is admitted again",
            Duration::from_secs(5),
            || async {
                server
                    .ingest_active(&format!("pipeline-{}", gated[PER_IP]))
                    .await
            },
        )
        .await;
    for key in &gated[..PER_IP] {
        assert!(!server.ingest_active(&format!("pipeline-{key}")).await);
    }
    stop.store(true, Ordering::Release);
    assert!(again.await.unwrap().connected);
    assert!(other.await.unwrap().connected);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admissions_per_stream_key_are_bounded_across_ips() {
    let auth = GatedAuth::new(&["churn", "churn-other"], &["churn"]);
    let server = TestServer::start_with_auth(
        auth.clone(),
        vec![plain("churn"), plain("churn-other")],
        Arc::new(MediaEngine::new()),
    )
    .await;
    let remote = server.remote;
    let stop = Arc::new(AtomicBool::new(false));
    // One pipeline's reconnect churn, each attempt from its own address.
    let holders: Vec<_> = (0..MAX_ADMISSIONS_PER_KEY as u8)
        .map(|index| hold_attach(from_ip(remote, 10 + index, "churn"), stop.clone()))
        .collect();
    server
        .wait_until("the per-key cap fills", Duration::from_secs(5), || async {
            auth.entered() == MAX_ADMISSIONS_PER_KEY
        })
        .await;

    // One more publisher on the key, from yet another address, is refused
    // without a lookup, while another key is admitted and a viewer burst on
    // the same key from one address is admitted in full: more readers than
    // the per-key cap (it counts publishers only) and than any small fixed
    // per-IP cap (it is the Owner's own per-IP limit, 64 by default).
    const READERS: usize = 9;
    let extra = blocking(move || {
        run_caller(
            from_ip(remote, 20, "churn"),
            Duration::from_secs(4),
            |ctx| ctx.report.disconnected,
        )
    });
    let other = hold_attach(from_ip(remote, 21, "churn-other"), stop.clone());
    let readers: Vec<_> = (0..READERS)
        .map(|_| hold_attach(direct(remote, "#!::r=churn,m=request", None), stop.clone()))
        .collect();
    let extra = extra.await;
    let (Some(up), Some(down)) = (extra.connected_at, extra.disconnected_at) else {
        panic!("the over-cap peer was not refused: {extra:?}");
    };
    assert!(
        down.duration_since(up) < Duration::from_secs(2),
        "refused at once, not by the lookup deadline: {extra:?}"
    );
    server
        .wait_until(
            "every reader on the capped key reaches the lookup",
            Duration::from_secs(4),
            || async { auth.entered() == MAX_ADMISSIONS_PER_KEY + READERS },
        )
        .await;
    server
        .wait_until(
            "another key is admitted",
            Duration::from_secs(4),
            || async { server.ingest_active("pipeline-churn-other").await },
        )
        .await;
    assert_eq!(
        auth.entered(),
        MAX_ADMISSIONS_PER_KEY + READERS,
        "the over-cap publisher never reached the lookup"
    );

    // The capped admissions proceed: one registers, the rest are duplicates.
    auth.open();
    server
        .wait_until(
            "a capped admission registers",
            Duration::from_secs(4),
            || async { server.ingest_active("pipeline-churn").await },
        )
        .await;
    stop.store(true, Ordering::Release);
    for holder in holders.into_iter().chain(readers) {
        assert!(holder.await.unwrap().connected);
    }
    assert!(other.await.unwrap().connected);
    server.stop().await;
}

/// A publisher on `key` from loopback address `127.0.0.<octet>`.
fn from_ip(remote: SocketAddr, octet: u8, key: &str) -> Attach {
    let config = CallerConfig::builder(remote)
        .ownership(SocketOwnership::Shared)
        .local_bind(SocketAddr::from(([127, 0, 0, octet], 0)))
        .session(session(&publish_id(key), None))
        .build()
        .expect("caller config");
    Attach::Direct(Box::new(config))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listener_exit_finishes_a_late_admission_and_a_restart_reuses_the_key() {
    let auth = GatedAuth::new(&["exit-a"], &["exit-a"]);
    let engine = Arc::new(MediaEngine::new());
    let server =
        TestServer::start_with_auth(auth.clone(), vec![plain("exit-a")], engine.clone()).await;
    let remote = server.remote;
    let stop = Arc::new(AtomicBool::new(false));
    let a = hold(remote, "exit-a", stop.clone());
    server
        .wait_until("A's lookup started", Duration::from_secs(5), || async {
            auth.entered() == 1
        })
        .await;

    // The loop exits and the Owner shuts down (its gauges are withdrawn) while
    // A's lookup is stalled; only then does the lookup answer.
    engine.shutdown_listeners();
    let owner = engine.listener_stats_handle();
    server
        .wait_until("the Owner shuts down", Duration::from_secs(4), || async {
            owner.ingress_owner.snapshot().owners == 0
        })
        .await;
    assert_eq!(auth.completed(), 0, "A's lookup was still stalled at exit");
    auth.open();
    tokio::time::timeout(Duration::from_secs(10), server.task)
        .await
        .expect("the listener stops")
        .expect("the listener task did not panic");
    stop.store(true, Ordering::Release);
    assert!(a.await.unwrap().connected);
    assert_eq!(
        auth.completed(),
        1,
        "the listener waited for the admission instead of aborting it"
    );
    assert!(
        !active(&engine, "pipeline-exit-a").await,
        "the late registration was finished at exit"
    );

    // A restarted listener on the same engine accepts the same key.
    let restarted =
        TestServer::start_with_engine(&["exit-a"], vec![plain("exit-a")], engine.clone()).await;
    let stop = Arc::new(AtomicBool::new(false));
    let again = publish(
        direct(restarted.remote, &publish_id("exit-a"), None),
        stop.clone(),
    );
    restarted
        .wait_until(
            "the restart publishes on the key",
            Duration::from_secs(5),
            || async { restarted.ring_progress("pipeline-exit-a").await > 0 },
        )
        .await;
    stop.store(true, Ordering::Release);
    assert!(again.await.unwrap().connected);
    restarted.stop().await;
}
