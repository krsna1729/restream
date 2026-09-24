use super::{CompioTcpPoller, CompioTcpStream, TcpConnectAttempt};
use crate::media::egress::scheduler::LeafKey;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

fn wait_for_read(
    poller: &mut CompioTcpPoller,
    fd: std::os::fd::RawFd,
    key: LeafKey,
    generation: u64,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "receive completion timed out"
        );
        let mut events = Vec::new();
        poller.poll_leaves(100, &mut events).unwrap();
        if events.iter().any(|event| {
            event.fd == fd && event.key == key && event.generation == generation && event.readable
        }) {
            return;
        }
    }
}

fn exercise_bounded_receive(ancillary_mode: bool) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let mut peer = TcpStream::connect(address).unwrap();
    let (client, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    peer.write_all(&vec![0x31; super::TRANSPORT_BUFFER_CAPACITY])
        .unwrap();

    let mut poller = CompioTcpPoller::new(4).unwrap();
    let mut stream = poller.adopt(client).unwrap();
    let fd = stream.raw_fd();
    let key = LeafKey(22);
    let generation = 51;
    let buffers = stream.io_buffers().unwrap();
    if ancillary_mode {
        stream.set_ancillary_mode();
    }
    poller
        .register_connection(fd, key, generation, &stream)
        .unwrap();

    while buffers.borrow().received.len() < super::TRANSPORT_BUFFER_CAPACITY {
        wait_for_read(&mut poller, fd, key, generation);
        assert!(buffers.borrow().received.len() <= super::TRANSPORT_BUFFER_CAPACITY);
        if buffers.borrow().received.len() < super::TRANSPORT_BUFFER_CAPACITY {
            stream.resume_receive();
        }
    }
    peer.write_all(&[0x42]).unwrap();
    let mut events = Vec::new();
    poller.poll_leaves(20, &mut events).unwrap();
    assert!(
        !events.iter().any(|event| event.fd == fd && event.readable),
        "receive worker must not read past the full queue"
    );
    assert_eq!(
        buffers.borrow().received.len(),
        super::TRANSPORT_BUFFER_CAPACITY,
        "receive completion must not exceed the bounded queue"
    );

    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 1);
    assert_eq!(
        buffers.borrow().received.len(),
        super::TRANSPORT_BUFFER_CAPACITY - 1
    );
    let mut events = Vec::new();
    poller.poll_leaves(20, &mut events).unwrap();
    assert!(
        !events.iter().any(|event| event.fd == fd && event.readable),
        "freeing queue room must not read until the next arm"
    );
    assert_eq!(
        buffers.borrow().received.len(),
        super::TRANSPORT_BUFFER_CAPACITY - 1,
        "a freed byte of room is not a new receive arm"
    );

    stream.resume_receive();
    wait_for_read(&mut poller, fd, key, generation);
    assert_eq!(
        buffers.borrow().received.len(),
        super::TRANSPORT_BUFFER_CAPACITY
    );
    assert_eq!(buffers.borrow().received.back(), Some(&0x42));

    poller.remove(fd).unwrap();
    drop(stream);
    drop(peer);
}

#[test]
fn receive_queue_is_bounded_and_partial_room_rearms_once() {
    exercise_bounded_receive(false);
    exercise_bounded_receive(true);
}

#[test]
fn compio_poller_reports_generation_tagged_connect_readiness() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || listener.accept().unwrap());
    let key = LeafKey(3);
    let mut poller = CompioTcpPoller::new(4).unwrap();
    let attempt = poller
        .start_connect(address, key, 7, Duration::from_secs(2))
        .unwrap();

    let (stream, ready) = match attempt {
        TcpConnectAttempt::Connected(stream) => (stream, None),
        TcpConnectAttempt::InProgress(stream) => {
            let mut events = Vec::new();
            assert_eq!(poller.poll_leaves(2_000, &mut events).unwrap(), 1);
            (stream, Some(events[0]))
        }
    };
    if let Some(event) = ready {
        assert_eq!(event.fd, stream.raw_fd());
        assert_eq!((event.key, event.generation), (key, 7));
        crate::media::egress::backends::tcp::connect_error(event.fd).unwrap();
        poller.remove(event.fd).unwrap();
    }
    poller
        .register_leaf(stream.raw_fd(), key, 8, super::TcpEgressInterest::WRITE)
        .unwrap();
    let (_command_tx, commands) = flume::unbounded();
    assert!(matches!(
        poller.wait_idle(&commands, Duration::from_secs(2)),
        crate::media::egress::shard::EgressShardIdleWake::BackendActivity
    ));
    let mut events = Vec::new();
    assert_eq!(
        poller.poll_leaves(0, &mut events).unwrap(),
        1,
        "idle-wait readiness must be preserved for the scheduled shard visit"
    );
    assert_eq!(events[0].generation, 8);
    assert_eq!(events[0].key, key);
    assert!(events[0].writable);
    poller.remove(stream.raw_fd()).unwrap();
    drop(stream);
    let (accepted, _) = server.join().unwrap();
    drop(accepted);
}

#[test]
fn ktls_buffered_record_reads_copy_partial_payload_with_type() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let peer = TcpStream::connect(address).unwrap();
    let (client, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    let runtime = compio::runtime::Runtime::new().unwrap();
    let client = runtime
        .enter(|| compio::net::TcpStream::from_std(client))
        .unwrap();
    let mut stream = CompioTcpStream::from_compio(client);
    let buffers = stream.io_buffers().unwrap();
    {
        let mut state = buffers.borrow_mut();
        state.ktls_active = true;
        state.received.extend(b"abc");
        state.record_type = Some((
            3,
            super::super::super::rtmp_connection::rtmp_ktls::RECORD_TYPE_ALERT,
        ));
    }

    let mut first = [0xaa; 2];
    assert_eq!(
        stream.read_record(&mut first).unwrap(),
        (
            2,
            super::super::super::rtmp_connection::rtmp_ktls::RECORD_TYPE_ALERT
        )
    );
    assert_eq!(&first, b"ab");
    let mut second = [0xaa; 4];
    assert_eq!(
        stream.read_record(&mut second).unwrap(),
        (
            1,
            super::super::super::rtmp_connection::rtmp_ktls::RECORD_TYPE_ALERT
        )
    );
    assert_eq!(&second, &[b'c', 0xaa, 0xaa, 0xaa]);
    {
        let mut state = buffers.borrow_mut();
        state.received.extend(b"XYZ");
        state.record_type = Some((
            3,
            super::super::super::rtmp_connection::rtmp_ktls::RECORD_TYPE_DATA,
        ));
    }
    let mut third = [0; 4];
    assert_eq!(
        stream.read_record(&mut third).unwrap(),
        (
            3,
            super::super::super::rtmp_connection::rtmp_ktls::RECORD_TYPE_DATA
        )
    );
    assert_eq!(&third[..3], b"XYZ");
    assert!(buffers.borrow().received.is_empty());
    assert!(buffers.borrow().record_type.is_none());
    drop(stream);
    drop(runtime);
    drop(peer);
}

#[test]
fn compio_completion_workers_roundtrip_connection_io() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = [0; 5];
        peer.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"hello");
        peer.write_all(b"world").unwrap();
    });

    let client = TcpStream::connect(address).unwrap();
    client.set_nonblocking(true).unwrap();
    let mut poller = CompioTcpPoller::new(4).unwrap();
    let mut stream = poller.adopt(client).unwrap();
    let fd = stream.raw_fd();
    let key = LeafKey(12);
    let generation = 41;
    poller
        .register_connection(fd, key, generation, &stream)
        .unwrap();
    assert_eq!(stream.write(b"hello").unwrap(), 5);
    poller
        .event_tx
        .send(super::TcpReadyLeaf {
            fd,
            key,
            generation: generation - 1,
            readable: true,
            writable: false,
        })
        .unwrap();
    let mut stale_events = Vec::new();
    assert_eq!(poller.poll_leaves(0, &mut stale_events).unwrap(), 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut readable = false;
    let mut writable = false;
    let mut response_read = false;
    let mut response = [0; 5];
    while !readable || !writable || !response_read {
        assert!(
            std::time::Instant::now() < deadline,
            "Compio completion workers did not report both directions"
        );
        let mut events = Vec::new();
        poller.poll_leaves(100, &mut events).unwrap();
        for event in events {
            assert_eq!((event.key, event.generation), (key, generation));
            readable |= event.readable;
            writable |= event.writable;
        }
        if readable && !response_read {
            assert_eq!(stream.read(&mut response).unwrap(), response.len());
            response_read = true;
        }
    }
    assert_eq!(&response, b"world");
    poller.remove(fd).unwrap();
    drop(stream);
    server.join().unwrap();
}
#[test]
fn command_and_socket_readiness_are_serviced_fairly() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || listener.accept().unwrap());
    let mut poller = CompioTcpPoller::new(4).unwrap();
    let client = TcpStream::connect(address).unwrap();
    client.set_nonblocking(true).unwrap();
    let stream = poller.adopt(client).unwrap();
    let fd = stream.raw_fd();
    let key = LeafKey(9);
    poller
        .register_leaf(fd, key, 11, super::TcpEgressInterest::WRITE)
        .unwrap();
    let (command_tx, commands) = flume::unbounded();
    const WAKE_COUNT: usize = 64;
    for _ in 0..WAKE_COUNT {
        command_tx
            .send(crate::media::egress::command::EgressCommand::FeedWake)
            .unwrap();
    }

    let mut commands_seen = 0;
    let mut readiness_seen = false;
    for _ in 0..4 {
        match poller.wait_idle(&commands, Duration::from_secs(2)) {
            crate::media::egress::shard::EgressShardIdleWake::BackendActivity => {
                let mut events = Vec::new();
                assert_eq!(poller.poll_leaves(0, &mut events).unwrap(), 1);
                assert_eq!((events[0].key, events[0].generation), (key, 11));
                assert!(events[0].writable);
                readiness_seen = true;
            }
            crate::media::egress::shard::EgressShardIdleWake::Command(
                crate::media::egress::command::EgressCommand::FeedWake,
            ) => commands_seen += 1,
            wake => panic!("unexpected idle wake: {wake:?}"),
        }
        if readiness_seen && commands_seen > 0 {
            break;
        }
    }
    assert!(readiness_seen);
    assert!(commands_seen > 0 && commands_seen < WAKE_COUNT);

    poller.remove(fd).unwrap();
    drop(stream);
    drop(command_tx);
    let (accepted, _) = server.join().unwrap();
    drop(accepted);
}

#[test]
fn active_completions_do_not_starve_pending_connect_readiness() {
    let active_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let active_address = active_listener.local_addr().unwrap();
    let active_server = thread::spawn(move || active_listener.accept().unwrap());
    let pending_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let pending_address = pending_listener.local_addr().unwrap();
    let pending_server = thread::spawn(move || pending_listener.accept().unwrap());

    let mut poller = CompioTcpPoller::new(4).unwrap();
    let active_client = TcpStream::connect(active_address).unwrap();
    active_client.set_nonblocking(true).unwrap();
    let active_stream = poller.adopt(active_client).unwrap();
    let active_fd = active_stream.raw_fd();
    let active_key = LeafKey(20);
    poller
        .register_connection(active_fd, active_key, 31, &active_stream)
        .unwrap();
    let (active_peer, _) = active_server.join().unwrap();
    poller
        .event_tx
        .send(super::TcpReadyLeaf {
            fd: active_fd,
            key: active_key,
            generation: 31,
            readable: true,
            writable: false,
        })
        .unwrap();

    let pending_client = TcpStream::connect(pending_address).unwrap();
    pending_client.set_nonblocking(true).unwrap();
    let pending_fd = pending_client.as_raw_fd();
    let pending_key = LeafKey(21);
    poller
        .register_leaf(pending_fd, pending_key, 32, super::TcpEgressInterest::WRITE)
        .unwrap();
    let (pending_peer, _) = pending_server.join().unwrap();

    let mut active_read_seen = false;
    let mut pending_write_seen = false;
    for _ in 0..2 {
        let mut events = Vec::new();
        poller.poll_leaves(2_000, &mut events).unwrap();
        for event in events {
            active_read_seen |= event.key == active_key && event.generation == 31 && event.readable;
            pending_write_seen |=
                event.key == pending_key && event.generation == 32 && event.writable;
        }
        if active_read_seen && pending_write_seen {
            break;
        }
    }
    assert!(active_read_seen, "active completion should be serviced");
    assert!(
        pending_write_seen,
        "pending connect readiness should survive active completions"
    );

    poller.remove(pending_fd).unwrap();
    poller.remove(active_fd).unwrap();
    drop(active_stream);
    drop(pending_client);
    drop(active_peer);
    drop(pending_peer);
}

#[test]
fn compio_poller_reports_simultaneous_read_and_write_readiness() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"x").unwrap();
    });
    let client = TcpStream::connect(address).unwrap();
    client.set_nonblocking(true).unwrap();
    let mut poller = CompioTcpPoller::new(4).unwrap();
    let stream = poller.adopt(client).unwrap();
    let fd = stream.raw_fd();
    let key = LeafKey(4);
    poller
        .register_leaf(fd, key, 13, super::TcpEgressInterest::READ_WRITE)
        .unwrap();
    server.join().unwrap();

    let mut events = Vec::new();
    assert_eq!(poller.poll_leaves(2_000, &mut events).unwrap(), 1);
    assert_eq!((events[0].key, events[0].generation), (key, 13));
    assert!(events[0].readable);
    assert!(events[0].writable);

    poller.remove(fd).unwrap();
    drop(stream);
}
