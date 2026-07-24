use super::*;
use std::os::unix::io::RawFd;

/// A connected AF_UNIX SOCK_STREAM pair: cheap, always epoll-writable once
/// connected, and needs no real network I/O — a faithful stand-in for a TCP
/// socket for exercising the real `LibcTcpPollOps` against a live kernel
/// epoll instance rather than a fake.
fn socketpair() -> (RawFd, RawFd) {
    let mut fds = [0 as RawFd; 2];
    let result = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(result, 0, "socketpair() must succeed in a test sandbox");
    (fds[0], fds[1])
}

fn close_fd(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

#[test]
fn real_epoll_reports_writable_for_a_connected_socket() {
    let (a, b) = socketpair();
    let mut poller = TcpEgressPoller::new(4).unwrap();

    poller
        .register_leaf(a, LeafKey(0), 1, TcpEgressInterest::WRITE)
        .unwrap();

    let mut ready = Vec::new();
    let count = poller.poll_leaves(1_000, &mut ready).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ready[0].fd, a);
    assert_eq!(ready[0].key, LeafKey(0));
    assert_eq!(ready[0].generation, 1);
    assert!(ready[0].writable);

    poller.remove(a).unwrap();
    close_fd(a);
    close_fd(b);
}

#[test]
fn real_epoll_reports_hangup_as_writable_after_peer_closes() {
    let (a, b) = socketpair();
    let mut poller = TcpEgressPoller::new(4).unwrap();

    poller
        .register_leaf(a, LeafKey(0), 1, TcpEgressInterest::WRITE)
        .unwrap();
    close_fd(b);

    let mut ready = Vec::new();
    poller.poll_leaves(1_000, &mut ready).unwrap();

    assert_eq!(ready.len(), 1);
    assert!(
        ready[0].writable,
        "a hangup must surface as a visit, not a silently dropped event"
    );

    poller.remove(a).unwrap();
    close_fd(a);
}

#[test]
fn removed_fd_stops_producing_events() {
    let (a, b) = socketpair();
    let mut poller = TcpEgressPoller::new(4).unwrap();

    poller
        .register_leaf(a, LeafKey(0), 1, TcpEgressInterest::WRITE)
        .unwrap();
    poller.remove(a).unwrap();

    let mut ready = Vec::new();
    poller.poll_leaves(50, &mut ready).unwrap();

    assert!(ready.is_empty(), "removed fd must not produce readiness");

    close_fd(a);
    close_fd(b);
}

#[test]
fn stale_registration_key_is_replaced_on_reregister() {
    let (a, b) = socketpair();
    let mut poller = TcpEgressPoller::new(4).unwrap();

    poller
        .register_leaf(a, LeafKey(0), 1, TcpEgressInterest::WRITE)
        .unwrap();
    // Same fd, new leaf generation — mirrors a leaf slot being reused after
    // the old leaf closed and a new one connected on the same fd integer.
    poller
        .register_leaf(a, LeafKey(0), 2, TcpEgressInterest::WRITE)
        .unwrap();

    let mut ready = Vec::new();
    poller.poll_leaves(1_000, &mut ready).unwrap();

    assert_eq!(ready.len(), 1);
    assert_eq!(
        ready[0].generation, 2,
        "poll must report the current generation, not the stale one"
    );

    poller.remove(a).unwrap();
    close_fd(a);
    close_fd(b);
}

// ---------------------------------------------------------------------------
// Fake ops: deterministic tests for registration bookkeeping and error
// propagation without depending on real kernel epoll behavior.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeTcpPollOps {
    create_result: std::cell::Cell<RawFd>,
    wait_events: std::cell::RefCell<Vec<(RawFd, u32)>>,
    ctl_calls: std::cell::RefCell<Vec<(c_int, RawFd, u32)>>,
    fail_next_wait: std::cell::Cell<bool>,
}

impl FakeTcpPollOps {
    fn new() -> Self {
        let ops = Self::default();
        ops.create_result.set(7);
        ops
    }
}

impl TcpPollOps for FakeTcpPollOps {
    fn create(&self) -> RawFd {
        self.create_result.get()
    }

    fn ctl_add(&self, _epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int {
        self.ctl_calls
            .borrow_mut()
            .push((libc::EPOLL_CTL_ADD, fd, events));
        0
    }

    fn ctl_mod(&self, _epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int {
        self.ctl_calls
            .borrow_mut()
            .push((libc::EPOLL_CTL_MOD, fd, events));
        0
    }

    fn ctl_del(&self, _epoll_fd: RawFd, fd: RawFd) -> c_int {
        self.ctl_calls
            .borrow_mut()
            .push((libc::EPOLL_CTL_DEL, fd, 0));
        0
    }

    fn wait(&self, _epoll_fd: RawFd, events: &mut [libc::epoll_event], _timeout_ms: i32) -> c_int {
        if self.fail_next_wait.get() {
            return -1;
        }
        let queued = self.wait_events.borrow();
        let count = queued.len().min(events.len());
        for (slot, (fd, ev)) in events.iter_mut().zip(queued.iter()).take(count) {
            slot.u64 = *fd as u64;
            slot.events = *ev;
        }
        count as c_int
    }

    fn close(&self, _epoll_fd: RawFd) -> c_int {
        0
    }

    fn error(&self, operation: &'static str) -> TcpEgressPollError {
        TcpEgressPollError::new(operation, -1, "fake failure".to_string())
    }
}

#[test]
fn register_leaf_uses_add_then_mod_on_reregistration() {
    let ops = FakeTcpPollOps::new();
    let mut poller = TcpEgressPoller::with_ops(4, ops).unwrap();

    poller
        .register_leaf(9, LeafKey(0), 1, TcpEgressInterest::WRITE)
        .unwrap();
    poller
        .register_leaf(9, LeafKey(0), 2, TcpEgressInterest::WRITE)
        .unwrap();

    let calls = poller.ops.ctl_calls.borrow();
    assert_eq!(calls[0].0, libc::EPOLL_CTL_ADD);
    assert_eq!(calls[1].0, libc::EPOLL_CTL_MOD);
}

#[test]
fn remove_unregistered_fd_is_a_noop() {
    let ops = FakeTcpPollOps::new();
    let mut poller = TcpEgressPoller::with_ops(4, ops).unwrap();

    assert!(poller.remove(42).is_ok());
    assert!(poller.ops.ctl_calls.borrow().is_empty());
}

#[test]
fn poll_leaves_ignores_events_for_deregistered_fds() {
    let ops = FakeTcpPollOps::new();
    ops.wait_events
        .borrow_mut()
        .push((5, libc::EPOLLOUT as u32));
    let mut poller = TcpEgressPoller::with_ops(4, ops).unwrap();
    // Never registered — simulates an event arriving for a socket that was
    // removed between the previous wait and this one.

    let mut ready = Vec::new();
    let count = poller.poll_leaves(0, &mut ready).unwrap();

    assert_eq!(count, 0);
    assert!(ready.is_empty());
}

#[test]
fn poll_leaves_propagates_wait_failure() {
    let ops = FakeTcpPollOps::new();
    ops.fail_next_wait.set(true);
    let mut poller = TcpEgressPoller::with_ops(4, ops).unwrap();

    let mut ready = Vec::new();
    let result = poller.poll_leaves(0, &mut ready);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().operation, "epoll_wait");
}
