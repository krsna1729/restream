use std::alloc::{GlobalAlloc, Layout, System};
use std::net::UdpSocket;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};

use restream_dataplane::udp::{
    UdpDriverDatagram, UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpDriver,
    UringUdpPoller,
};
use restream_dataplane::{FeedCursor, MediaArena, MediaRing, ReadyQueue, TxPool};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn dataplane_hot_paths_do_not_allocate() {
    let mut queue = ReadyQueue::new(64, 64).unwrap();
    ALLOCATIONS.store(0, Ordering::Relaxed);

    for _ in 0..10_000 {
        assert!(queue.enqueue(0));
        assert_eq!(queue.pop(), Some(0));
    }

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);

    let mut ring = MediaRing::new(
        MediaArena::new(128, 256).unwrap(),
        64,
        16 * 1024,
        std::time::Duration::from_secs(60),
    )
    .unwrap();
    let payload = [7_u8; 128];
    ALLOCATIONS.store(0, Ordering::Relaxed);

    for sequence in 0..10_000_u64 {
        let reference = ring.arena_mut().acquire_copy(&payload).unwrap();
        ring.push(reference, std::time::Instant::now(), sequence % 30 == 0)
            .unwrap();
        let cursor = FeedCursor::new(ring.epoch(), ring.next_sequence().saturating_sub(1));
        let current = ring.read_cursor(cursor).unwrap();
        assert!(ring.retain(current));
        assert!(ring.release(current));
    }

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);

    let mut tx = TxPool::new(8, 256).unwrap();
    ALLOCATIONS.store(0, Ordering::Relaxed);
    for _ in 0..10_000 {
        let lease = tx.acquire().unwrap();
        tx.slot_mut(lease).unwrap()[0] = 1;
        assert!(tx.submit(lease));
        assert!(tx.complete(lease));
        assert!(tx.release(lease));
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut poller = match UringUdpPoller::new_fixed(1, 32) {
        Ok(poller) => poller,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(error) => panic!("fixed-file io_uring unavailable: {error}"),
    };
    poller
        .register_fixed(sender.as_raw_fd(), 0, 1, UdpInterest::READ)
        .unwrap();
    let destination = receiver.local_addr().unwrap();
    let mut ready = [UdpReadyEvent {
        fd: -1,
        slot: 0,
        generation: 0,
        readable: false,
        writable: false,
    }];
    let mut completions = [UdpSendCompletion {
        slot: 0,
        generation: 0,
        result: 0,
    }];
    let payload = [7_u8; 4];
    ALLOCATIONS.store(0, Ordering::Relaxed);

    for _ in 0..64 {
        poller
            .submit_send(sender.as_raw_fd(), 0, 1, destination, &payload)
            .unwrap();
        loop {
            poller.poll(std::time::Duration::ZERO, &mut ready).unwrap();
            if poller.drain_send_completions(&mut completions) == 1 {
                assert_eq!(completions[0].result, payload.len() as i32);
                break;
            }
        }
        let mut received = [0_u8; 4];
        assert_eq!(receiver.recv_from(&mut received).unwrap().0, payload.len());
        assert_eq!(received, payload);
    }

    let driver_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut driver = match UringUdpDriver::new_fixed(1, 32, 8, 8, 2_048) {
        Ok(driver) => driver,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(error) => panic!("driver io_uring unavailable: {error}"),
    };
    driver
        .register_fixed(driver_sock.as_raw_fd(), 0, 1, UdpInterest::READ_WRITE)
        .unwrap();
    let mut driver_ready = [UdpReadyEvent {
        fd: -1,
        slot: 0,
        generation: 0,
        readable: false,
        writable: false,
    }];
    let mut driver_datagrams = [UdpDriverDatagram {
        slot: 0,
        buffer_id: 0,
        offset: 0,
        len: 0,
        peer: "0.0.0.0:0".parse().unwrap(),
    }; 4];
    ALLOCATIONS.store(0, Ordering::Relaxed);
    for _ in 0..100 {
        let _ = driver
            .poll(
                std::time::Duration::ZERO,
                &mut driver_ready,
                &mut driver_datagrams,
            )
            .unwrap();
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
