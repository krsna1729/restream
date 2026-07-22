use super::srt_egress_poller::*;
use super::sys::{SRT_EPOLL_ERR, SRT_EPOLL_OUT, SRTSOCKET};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::rc::Rc;

#[derive(Clone)]
struct FakePollOps {
    state: Rc<RefCell<FakePollState>>,
}

#[derive(Default)]
struct FakePollState {
    next_eid: c_int,
    created: Vec<c_int>,
    added: Vec<(c_int, SRTSOCKET, c_int)>,
    updated: Vec<(c_int, SRTSOCKET, c_int)>,
    removed: Vec<(c_int, SRTSOCKET)>,
    released: Vec<c_int>,
    waits: VecDeque<FakeWaitResult>,
    fail_create: bool,
}

struct FakeWaitResult {
    read: Vec<SRTSOCKET>,
    write: Vec<SRTSOCKET>,
}

impl Default for FakePollOps {
    fn default() -> Self {
        Self {
            state: Rc::new(RefCell::new(FakePollState {
                next_eid: 7,
                ..FakePollState::default()
            })),
        }
    }
}

impl FakePollOps {
    fn push_wait(&self, read: Vec<SRTSOCKET>, write: Vec<SRTSOCKET>) {
        self.state
            .borrow_mut()
            .waits
            .push_back(FakeWaitResult { read, write });
    }
}

impl SrtPollOps for FakePollOps {
    fn create(&self) -> c_int {
        let mut state = self.state.borrow_mut();
        if state.fail_create {
            return -1;
        }
        let eid = state.next_eid;
        state.next_eid += 1;
        state.created.push(eid);
        eid
    }

    fn add_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int {
        self.state.borrow_mut().added.push((eid, socket, events));
        0
    }

    fn update_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int {
        self.state.borrow_mut().updated.push((eid, socket, events));
        0
    }

    fn remove_usock(&self, eid: c_int, socket: SRTSOCKET) -> c_int {
        self.state.borrow_mut().removed.push((eid, socket));
        0
    }

    fn wait(
        &self,
        _eid: c_int,
        readfds: &mut [SRTSOCKET],
        read_count: &mut c_int,
        writefds: &mut [SRTSOCKET],
        write_count: &mut c_int,
        _timeout_ms: i64,
    ) -> c_int {
        let result = self
            .state
            .borrow_mut()
            .waits
            .pop_front()
            .unwrap_or(FakeWaitResult {
                read: Vec::new(),
                write: Vec::new(),
            });
        let read_len = result.read.len().min(readfds.len());
        let write_len = result.write.len().min(writefds.len());
        readfds[..read_len].copy_from_slice(&result.read[..read_len]);
        writefds[..write_len].copy_from_slice(&result.write[..write_len]);
        *read_count = read_len as c_int;
        *write_count = write_len as c_int;
        (read_len + write_len) as c_int
    }

    fn release(&self, eid: c_int) -> c_int {
        self.state.borrow_mut().released.push(eid);
        0
    }

    fn error(&self, operation: &'static str) -> SrtEgressPollError {
        SrtEgressPollError {
            operation,
            code: 1234,
            message: "fake error".to_string(),
        }
    }
}

#[test]
fn creates_and_releases_epoll_container() {
    let ops = FakePollOps::default();
    {
        let _poller = SrtEgressPoller::with_ops(8, ops.clone()).unwrap();
        assert_eq!(ops.state.borrow().created, vec![7]);
    }

    assert_eq!(ops.state.borrow().released, vec![7]);
}

#[test]
fn register_adds_then_updates_socket_interest() {
    let ops = FakePollOps::default();
    let mut poller = SrtEgressPoller::with_ops(8, ops.clone()).unwrap();

    poller.register(42, SrtEgressInterest::WRITE).unwrap();
    poller.register(42, SrtEgressInterest::WRITE).unwrap();

    let state = ops.state.borrow();
    assert_eq!(state.added, vec![(7, 42, SRT_EPOLL_OUT | SRT_EPOLL_ERR)]);
    assert_eq!(state.updated, vec![(7, 42, SRT_EPOLL_OUT | SRT_EPOLL_ERR)]);
}

#[test]
fn remove_deregisters_once_before_socket_close() {
    let ops = FakePollOps::default();
    let mut poller = SrtEgressPoller::with_ops(8, ops.clone()).unwrap();

    poller.register(42, SrtEgressInterest::WRITE).unwrap();
    poller.remove(42).unwrap();
    poller.remove(42).unwrap();

    assert_eq!(ops.state.borrow().removed, vec![(7, 42)]);
}

#[test]
fn poll_returns_bounded_writable_events() {
    let ops = FakePollOps::default();
    ops.push_wait(Vec::new(), vec![10, 11, 12]);
    let mut poller = SrtEgressPoller::with_ops(2, ops).unwrap();
    let mut ready = Vec::new();

    let count = poller.poll(25, &mut ready).unwrap();

    assert_eq!(count, 2);
    assert_eq!(
        ready,
        vec![
            SrtReadySocket {
                socket: 10,
                writable: true,
            },
            SrtReadySocket {
                socket: 11,
                writable: true,
            },
        ]
    );
}

#[test]
fn poll_merges_error_style_read_and_write_reports() {
    let ops = FakePollOps::default();
    ops.push_wait(vec![42], vec![42]);
    let mut poller = SrtEgressPoller::with_ops(8, ops).unwrap();
    let mut ready = Vec::new();

    poller.poll(25, &mut ready).unwrap();

    assert_eq!(
        ready,
        vec![SrtReadySocket {
            socket: 42,
            writable: true,
        }]
    );
}

#[test]
fn create_failure_reports_operation() {
    let ops = FakePollOps::default();
    ops.state.borrow_mut().fail_create = true;

    let error = match SrtEgressPoller::with_ops(8, ops) {
        Ok(_) => panic!("poller construction should fail"),
        Err(error) => error,
    };

    assert_eq!(error.operation, "srt_epoll_create");
    assert_eq!(error.code, 1234);
}
