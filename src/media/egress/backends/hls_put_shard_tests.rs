use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::*;
use crate::media::egress::backends::compio_tcp::CompioTcpPoller;
use crate::media::egress::command::{FeedId, ShardId};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    body: Vec<u8>,
    connection: usize,
}

/// A scripted HTTP/1.1 origin on std threads: `answer(nth request, target)`
/// gives the status and whether to close the connection after answering.
struct Origin {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Origin {
    fn start(answer: impl Fn(usize, &str) -> (u16, bool) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let answer = Arc::new(answer);
        let seen_for_accept = seen.clone();
        std::thread::spawn(move || {
            for (connection, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let (seen, answer) = (seen_for_accept.clone(), answer.clone());
                std::thread::spawn(move || serve(stream, connection, &seen, &*answer));
            }
        });
        Self { port, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        crate::sync::lock(&self.seen).clone()
    }

    fn wait_for(&self, what: &str, done: impl Fn(&[Seen]) -> bool) -> Vec<Seen> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = self.seen();
            if done(&seen) {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {seen:#?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn serve(
    mut stream: TcpStream,
    connection: usize,
    seen: &Mutex<Vec<Seen>>,
    answer: &(dyn Fn(usize, &str) -> (u16, bool) + Send + Sync),
) {
    let mut buffer = Vec::new();
    loop {
        let head_end = loop {
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            }
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        let mut words = head.split_whitespace();
        let (method, target) = (
            words.next().unwrap().to_string(),
            words.next().unwrap().to_string(),
        );
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .map_or(0, |value| value.trim().parse().unwrap());
        while buffer.len() < head_end + length {
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            }
        }
        let body = buffer[head_end..head_end + length].to_vec();
        buffer.drain(..head_end + length);
        let nth = {
            let mut seen = crate::sync::lock(seen);
            seen.push(Seen {
                method,
                target: target.clone(),
                body,
                connection,
            });
            seen.len() - 1
        };
        let (status, close) = answer(nth, &target);
        let connection_header = if close { "Connection: close\r\n" } else { "" };
        let response =
            format!("HTTP/1.1 {status} X\r\n{connection_header}Content-Length: 0\r\n\r\n");
        if stream.write_all(response.as_bytes()).is_err() || close {
            return;
        }
    }
}

struct Output {
    handle: EgressShardHandle,
    store: Arc<HlsStore>,
    bytes_sent: Arc<AtomicU64>,
    terminated: Arc<AtomicBool>,
}

fn start_output(port: u16) -> Output {
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.5, Bytes::from_static(b"segment-zero"));
    let config = EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5)).unwrap();
    let backend_store = store.clone();
    let handle = EgressShardHandle::spawn_with(ShardId::new(0), config, move || {
        HlsPutShardBackend::new(
            CompioTcpPoller::new(64).unwrap(),
            backend_store,
            crate::media::egress::tls::rustls_client_config(),
            8,
        )
        .unwrap()
    });
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let terminated = Arc::new(AtomicBool::new(false));
    handle
        .try_send(EgressCommand::Add(OutputSpec {
            id: OutputId::new("out-hls"),
            generation: 1,
            feed: FeedId::new("hls:pipe"),
            protocol: ProtocolSpec::HlsPut {
                url: format!("http://127.0.0.1:{port}/live/out.m3u8"),
            },
            policy: LeafPolicy::default(),
            progress: EgressProgressSink {
                bytes_sent: Some(bytes_sent.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                ..Default::default()
            },
        }))
        .unwrap();
    Output {
        handle,
        store,
        bytes_sent,
        terminated,
    }
}

fn segment_targets(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .filter(|request| request.target.ends_with(".ts"))
        .map(|request| request.target.rsplit_once('-').unwrap().1.to_string())
        .collect()
}

/// A whole session on a real shard thread: segment, playlist, a second
/// segment on the next publish, all on one persistent connection, then the
/// end playlist after the output is removed.
#[test]
fn a_session_uploads_on_one_connection_and_ends_with_endlist() {
    let origin = Origin::start(|_, _| (200, false));
    let output = start_output(origin.port);

    origin.wait_for("segment and playlist", |seen| seen.len() >= 2);
    output
        .store
        .push_segment(1.5, Bytes::from_static(b"segment-one"));
    output.handle.try_send(EgressCommand::FeedWake).unwrap();
    origin.wait_for("second segment and playlist", |seen| seen.len() >= 4);
    output
        .handle
        .try_send(EgressCommand::Remove(OutputId::new("out-hls")))
        .unwrap();
    let seen = origin.wait_for("end playlist", |seen| {
        seen.last()
            .is_some_and(|last| last.body.ends_with(b"#EXT-X-ENDLIST\n"))
    });

    assert!(seen.iter().all(|request| request.method == "PUT"));
    assert_eq!(segment_targets(&seen), ["0.ts", "1.ts"]);
    let order: Vec<bool> = seen
        .iter()
        .map(|request| request.target.ends_with(".ts"))
        .collect();
    assert_eq!(
        order,
        [true, false, true, false, false],
        "segment, playlist, ..."
    );
    assert_eq!(seen[0].body, b"segment-zero");
    assert!(
        seen.iter()
            .all(|request| request.connection == seen[0].connection),
        "one persistent connection"
    );
    assert!(output.bytes_sent.load(Ordering::Relaxed) >= 23);
    assert!(!output.terminated.load(Ordering::Relaxed));
    assert!(!output.handle.shutdown_and_join().panicked);
}

/// A 401 (YouTube: the cid expired) ends the leaf without a retry and
/// reports it, so the egress task records the failure.
#[test]
fn a_rejection_ends_the_leaf_and_reports_it() {
    let origin = Origin::start(|_, _| (401, false));
    let output = start_output(origin.port);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.terminated.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline, "the rejection was not reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(origin.seen().len(), 1, "no retry after a 401");
    assert!(!output.handle.shutdown_and_join().panicked);
}

/// A 503 is retried on a fresh connection and the session recovers.
#[test]
fn a_server_error_is_retried_on_a_new_connection() {
    let origin = Origin::start(|nth, _| (if nth == 0 { 503 } else { 200 }, false));
    let output = start_output(origin.port);
    let seen = origin.wait_for("the retry and the playlist", |seen| seen.len() >= 3);
    assert_eq!(
        segment_targets(&seen[..2]),
        ["0.ts", "0.ts"],
        "the same segment again"
    );
    assert_ne!(seen[0].connection, seen[1].connection, "a fresh connection");
    assert!(!output.terminated.load(Ordering::Relaxed));
    assert!(!output.handle.shutdown_and_join().panicked);
}

/// An origin that closes after every response still gets every upload,
/// each on a new connection and without a failed attempt.
#[test]
fn an_origin_that_closes_after_each_response_gets_every_upload() {
    let origin = Origin::start(|_, _| (200, true));
    let output = start_output(origin.port);
    origin.wait_for("segment and playlist", |seen| seen.len() >= 2);
    output
        .store
        .push_segment(1.5, Bytes::from_static(b"segment-one"));
    output.handle.try_send(EgressCommand::FeedWake).unwrap();
    let seen = origin.wait_for("second segment and playlist", |seen| seen.len() >= 4);
    assert_eq!(segment_targets(&seen), ["0.ts", "1.ts"]);
    let connections: std::collections::BTreeSet<_> =
        seen.iter().map(|request| request.connection).collect();
    assert_eq!(
        connections.len(),
        seen.len(),
        "a new connection per request"
    );
    assert!(!output.handle.shutdown_and_join().panicked);
}

fn backend(capacity: usize) -> HlsPutShardBackend<CompioTcpPoller> {
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.5, Bytes::from_static(b"segment-zero"));
    HlsPutShardBackend::new(
        CompioTcpPoller::new(64).unwrap(),
        store,
        crate::media::egress::tls::rustls_client_config(),
        capacity,
    )
    .unwrap()
}

fn hls_spec(id: &str, port: u16) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: FeedId::new("hls:pipe"),
        protocol: ProtocolSpec::HlsPut {
            url: format!("http://127.0.0.1:{port}/live/{id}.m3u8"),
        },
        policy: LeafPolicy::default(),
        progress: EgressProgressSink::default(),
    }
}

/// A DNS answer for a removed output must not connect the output that
/// reuses its slot (both start their lookups at token 1): it would send
/// the new output's segments and stream key to the old output's origin.
#[test]
fn a_removed_outputs_late_dns_answer_does_not_reach_the_slots_next_output() {
    let mut backend = backend(1);
    backend.on_command(EgressCommand::Add(hls_spec("old", 1)));
    let old = backend.by_output[&OutputId::new("old")];
    backend.close_slot(old);
    backend.on_command(EgressCommand::Add(hls_spec("new", 2)));
    let new = backend.by_output[&OutputId::new("new")];
    let new_token = match backend.slots.get(new).map(|slot| &slot.conn) {
        Some(Conn::Resolving { token, .. }) => *token,
        _ => panic!("the new output resolves first"),
    };

    backend.resolved(Resolved {
        key: old,
        token: new_token,
        addr: Some("127.0.0.1:9".parse().unwrap()),
    });

    assert!(
        matches!(
            backend.slots.get(new).map(|slot| &slot.conn),
            Some(Conn::Resolving { .. })
        ),
        "the old output's answer connected the new output"
    );
}

/// A lookup or connect that outlives its deadline fails the waiting
/// request (and is retried) instead of holding the slot until the kernel
/// gives up, with the shard re-arming an already-expired timer meanwhile.
#[test]
fn an_expired_resolve_fails_the_waiting_request() {
    let mut backend = backend(1);
    backend.on_command(EgressCommand::Add(hls_spec("slow-dns", 1)));
    let key = backend.by_output[&OutputId::new("slow-dns")];
    let past = Instant::now() - Duration::from_secs(1);
    if let Some(slot) = backend.slots.get_mut(key) {
        assert!(
            slot.waiting_request.is_some(),
            "the first segment waits for a connection"
        );
        slot.conn = Conn::Resolving {
            token: u64::MAX,
            deadline: past,
        };
    }

    backend.drive(key, Instant::now());

    let slot = backend.slots.get(key).unwrap();
    assert!(
        !matches!(
            slot.conn,
            Conn::Resolving {
                token: u64::MAX,
                ..
            }
        ),
        "the expired lookup still holds the slot"
    );
    assert!(
        slot.waiting_request.is_none(),
        "the request was failed and handed back"
    );
}

/// Remove then shutdown (the last output on a pipeline): while the end
/// playlist is in flight the backend must report work, or the shard's
/// "idle while draining" check stops it and EXT-X-ENDLIST is lost.
#[test]
fn a_drain_stays_alive_while_an_end_playlist_is_in_flight() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accepts and reads, never answers: every exchange stays in flight.
    let _silent = std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().take(4).collect();
        std::thread::sleep(Duration::from_secs(5));
        drop(held);
    });
    let mut backend = backend(1);
    backend.on_command(EgressCommand::Add(hls_spec("ending", port)));
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend
        .slots
        .get(backend.by_output[&OutputId::new("ending")])
        .is_some_and(|slot| slot.exchange.is_none())
    {
        assert!(Instant::now() < deadline, "the first upload never started");
        backend.on_ready();
        std::thread::sleep(Duration::from_millis(5));
    }
    backend.on_command(EgressCommand::Remove(OutputId::new("ending")));
    backend.on_command(EgressCommand::Shutdown);

    assert!(!backend.slots.is_empty());
    assert_ne!(
        backend.on_ready(),
        EgressShardCommandEffect::Continue,
        "a draining shard with an end playlist outstanding reported no work"
    );
    backend.on_shutdown();
}
