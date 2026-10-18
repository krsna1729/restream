use std::cell::RefCell;
use std::future::poll_fn;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures_util::future::join;
use moq_net::transport::poll::{RecvStream as _, SendStream as _, Session as _};
use moq_net::{Client, Hop, Server, Timestamp};
use web_transport_trait::Error as _;
use web_transport_trait::poll::{SendStream as _, Session as _};

use super::session::MoqRecvStream;
use super::test_support::{Loopback, loopback_client_tls, run};
use super::*;

fn origin(hop: u64) -> moq_net::origin::Producer {
    let (producer, driver) =
        moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
    compio::runtime::spawn(run_driver(driver)).detach();
    producer
}

/// moq-net's lite handshake over `client`/`server` (`connect_lite` /
/// `accept_request_lite`), then one frame published on the client and read
/// back through the server's subscription.
pub(super) async fn moq_lite_round_trip(client: MoqSession, server: MoqSession) {
    let publisher = origin(1);
    let broadcast = publisher.create_broadcast("room").unwrap();
    let track = broadcast.create_track("video", None).unwrap();
    broadcast.announce(Default::default()).unwrap();
    let subscriber = origin(2);

    let dial = async {
        let (session, driver) = Client::new()
            .with_publisher(publisher.consume())
            .with_path("live/key")
            .connect_lite(Instant::now(), client)
            .await
            .expect("connect_lite");
        compio::runtime::spawn(run_driver(driver)).detach();
        session
    };
    let answer = async {
        let handshake = Server::new()
            .with_subscriber(subscriber.clone())
            .accept_request_lite(Instant::now(), server)
            .await
            .expect("accept_request_lite");
        assert_eq!(handshake.path(), "live/key");
        let (session, driver) = handshake.ok().await.expect("handshake ok");
        compio::runtime::spawn(run_driver(driver)).detach();
        session
    };
    let _sessions = join(dial, answer).await;

    let consumer = subscriber.consume();
    consumer.routed("room").await.expect("routed");
    let remote = consumer
        .request_broadcast("room")
        .await
        .expect("broadcast resolves");
    let mut subscription = remote
        .track("video")
        .unwrap()
        .subscribe(None)
        .await
        .expect("subscribe");
    // The subscription is read while the publisher waits for its first
    // remote consumer, as a live player would.
    let read = async {
        let mut received = subscription.recv_group().await.unwrap().expect("a group");
        received.read_frame().await.unwrap().expect("a frame")
    };
    let write = async {
        track.used().await.unwrap();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(Timestamp::ZERO, &b"keyframe"[..])
            .unwrap();
        group.finish().unwrap();
    };
    let (frame, ()) = join(read, write).await;
    assert_eq!(frame.payload.to_vec(), b"keyframe");
}

pub(super) async fn read_all(recv: &mut MoqRecvStream) -> Result<Vec<u8>, TransportError> {
    let mut all = Vec::new();
    let mut buf = [0; 3];
    while let Some(read) = recv.read(&mut buf).await? {
        all.extend_from_slice(&buf[..read]);
    }
    Ok(all)
}

/// A uni stream and a bi stream exchange bytes, FIN included.
pub(super) async fn exchange_streams(client: &mut MoqSession, server: &mut MoqSession) {
    let mut uni = client.open_uni().await.unwrap();
    uni.write(b"hello").await.unwrap();
    uni.finish().unwrap();
    let mut accepted = server.accept_uni().await.unwrap();
    assert_eq!(read_all(&mut accepted).await.unwrap(), b"hello");

    let (mut send, mut recv) = client.open_bi().await.unwrap();
    send.write(b"ping").await.unwrap();
    let (mut reply, mut request) = server.accept_bi().await.unwrap();
    let mut ping = [0; 4];
    let mut filled = 0;
    while filled < ping.len() {
        filled += request.read(&mut ping[filled..]).await.unwrap().unwrap();
    }
    assert_eq!(&ping, b"ping");
    reply.write(b"pong").await.unwrap();
    reply.finish().unwrap();
    assert_eq!(read_all(&mut recv).await.unwrap(), b"pong");
}

/// Dropping an unfinished send stream resets it with code 0.
pub(super) async fn dropped_stream_is_reset(client: &mut MoqSession, server: &mut MoqSession) {
    let mut uni = client.open_uni().await.unwrap();
    uni.write(b"partial").await.unwrap();
    let mut accepted = server.accept_uni().await.unwrap();
    drop(uni);
    assert!(matches!(
        read_all(&mut accepted).await,
        Err(TransportError::StreamReset(0))
    ));
}

/// `close` resolves the local `poll_closed` at once and the peer's with the
/// code and reason.
pub(super) async fn peer_close_is_observed(mut client: MoqSession, mut server: MoqSession) {
    client.close(7, "bye");
    let local = poll_fn(|cx| client.poll_closed(cx)).await;
    assert!(matches!(
        local,
        TransportError::Connection(ConnectionError::LocallyClosed)
    ));
    let remote = poll_fn(|cx| server.poll_closed(cx)).await;
    assert_eq!(
        remote.session_error(),
        Some((7, "bye".to_owned())),
        "{remote}"
    );
    assert!(server.open_uni().await.is_err());
}

/// The connection outlives a dropped clone and closes with the last one.
pub(super) async fn last_drop_closes(mut client: MoqSession, server: MoqSession) {
    let mut clone = server.clone();
    drop(server);
    exchange_streams(&mut client, &mut clone).await;
    drop(clone);
    let reason = poll_fn(|cx| client.poll_closed(cx)).await;
    assert_eq!(reason.session_error(), Some((0, String::new())), "{reason}");
}

#[test]
fn the_lite_alpns_are_moq_nets_without_legacy_negotiation() {
    assert_eq!(CLIENT_ALPNS, &moq_net::ALPNS[..4]);
    assert_eq!(&SERVER_ALPNS[..4], CLIENT_ALPNS);
    assert_eq!(SERVER_ALPNS[4], "h3");
}

#[test]
fn native_moq_lite_session_round_trips_a_frame() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.native().await;
        assert_eq!(accepted.mode, TransportMode::Native);
        assert_eq!(
            (accepted.path.as_str(), accepted.query.as_deref()),
            ("", None)
        );
        assert_eq!(client.protocol(), Some("moq-lite-06"));
        assert_eq!(accepted.session.protocol(), Some("moq-lite-06"));
        moq_lite_round_trip(client, accepted.session).await;
    });
}

#[test]
fn native_streams_exchange_bytes() {
    run(async {
        let loopback = Loopback::new().await;
        let (mut client, accepted) = loopback.native().await;
        let mut server = accepted.session;
        exchange_streams(&mut client, &mut server).await;
        dropped_stream_is_reset(&mut client, &mut server).await;
    });
}

#[test]
fn native_peer_close_reaches_poll_closed() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.native().await;
        peer_close_is_observed(client, accepted.session).await;
    });
}

#[test]
fn native_session_closes_when_its_last_clone_drops() {
    run(async {
        let loopback = Loopback::new().await;
        let (client, accepted) = loopback.native().await;
        last_drop_closes(client, accepted.session).await;
    });
}

#[test]
fn connect_refuses_a_session_without_a_moq_alpn() {
    run(async {
        let loopback = Loopback::new().await;
        let dialed = connect(
            &loopback.client,
            loopback.addr,
            test_support::LOOPBACK_SERVER_NAME,
            loopback_client_tls(&["h3"]),
        )
        .await;
        assert!(matches!(dialed, Err(TransportError::Handshake(_))));
    });
}

/// Records each `now`; asks for a 5 ms deadline at a time and finishes once
/// three have passed.
struct Ticks {
    seen: Rc<RefCell<Vec<Instant>>>,
    next: Option<Instant>,
    fired: u32,
}

impl moq_net::time::Driver for Ticks {
    fn poll(
        &mut self,
        now: Instant,
        _waiter: &kio::Waiter,
    ) -> Result<Option<Instant>, moq_net::Error> {
        self.seen.borrow_mut().push(now);
        if self.next.is_some_and(|next| now >= next) {
            self.fired += 1;
            self.next = None;
        }
        if self.fired == 3 {
            return Err(moq_net::Error::Closed);
        }
        Ok(Some(
            *self.next.get_or_insert(now + Duration::from_millis(5)),
        ))
    }
}

#[test]
fn run_driver_sleeps_until_each_deadline_with_monotonic_time() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let ticks = Ticks {
        seen: seen.clone(),
        next: None,
        fired: 0,
    };
    let started = Instant::now();
    let error = run(run_driver(ticks));
    assert!(matches!(error, moq_net::Error::Closed));
    assert!(started.elapsed() >= Duration::from_millis(15));
    let seen = seen.borrow();
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]));
}
