#![allow(dead_code)]

use std::collections::HashMap;
use std::os::raw::c_int;

use super::socket::last_srt_error;
use super::sys::{
    SRT_EPOLL_ERR, SRT_EPOLL_OUT, SRTSOCKET, srt_epoll_add_usock, srt_epoll_create,
    srt_epoll_release, srt_epoll_remove_usock, srt_epoll_update_usock, srt_epoll_wait,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct SrtEgressInterest {
    pub writable: bool,
}

impl SrtEgressInterest {
    pub const WRITE: Self = Self { writable: true };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SrtReadySocket {
    pub socket: SRTSOCKET,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SrtEgressPollError {
    pub operation: &'static str,
    pub code: c_int,
    pub message: String,
}

impl SrtEgressPollError {
    fn new(operation: &'static str, code: c_int, message: String) -> Self {
        Self {
            operation,
            code,
            message,
        }
    }
}

pub(super) struct SrtEgressPoller<O = LibSrtPollOps>
where
    O: SrtPollOps,
{
    eid: c_int,
    ops: O,
    readfds: Vec<SRTSOCKET>,
    writefds: Vec<SRTSOCKET>,
    registered: HashMap<SRTSOCKET, SrtEgressInterest>,
}

impl SrtEgressPoller<LibSrtPollOps> {
    pub(super) fn new(max_events: usize) -> Result<Self, SrtEgressPollError> {
        Self::with_ops(max_events, LibSrtPollOps)
    }
}

impl<O> SrtEgressPoller<O>
where
    O: SrtPollOps,
{
    fn with_ops(max_events: usize, ops: O) -> Result<Self, SrtEgressPollError> {
        let eid = ops.create();
        if eid < 0 {
            return Err(ops.error("srt_epoll_create"));
        }

        let capacity = max_events.max(1);
        Ok(Self {
            eid,
            ops,
            readfds: vec![0; capacity],
            writefds: vec![0; capacity],
            registered: HashMap::new(),
        })
    }

    pub(super) fn register(
        &mut self,
        socket: SRTSOCKET,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        let events = events_for(interest);
        let result = if self.registered.contains_key(&socket) {
            self.ops.update_usock(self.eid, socket, events)
        } else {
            self.ops.add_usock(self.eid, socket, events)
        };

        if result < 0 {
            return Err(self.ops.error("srt_epoll_register"));
        }

        self.registered.insert(socket, interest);
        Ok(())
    }

    pub(super) fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        if !self.registered.contains_key(&socket) {
            return Ok(());
        }

        if self.ops.remove_usock(self.eid, socket) < 0 {
            return Err(self.ops.error("srt_epoll_remove_usock"));
        }

        self.registered.remove(&socket);
        Ok(())
    }

    pub(super) fn poll(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadySocket>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        let mut read_count = self.readfds.len() as c_int;
        let mut write_count = self.writefds.len() as c_int;
        let result = self.ops.wait(
            self.eid,
            &mut self.readfds,
            &mut read_count,
            &mut self.writefds,
            &mut write_count,
            timeout_ms,
        );

        if result < 0 {
            return Err(self.ops.error("srt_epoll_wait"));
        }

        for socket in self.writefds.iter().take(write_count.max(0) as usize) {
            if let Some(existing) = ready.iter_mut().find(|event| event.socket == *socket) {
                existing.writable = true;
            } else {
                ready.push(SrtReadySocket {
                    socket: *socket,
                    writable: true,
                });
            }
        }

        for socket in self.readfds.iter().take(read_count.max(0) as usize) {
            if !ready.iter().any(|event| event.socket == *socket) {
                ready.push(SrtReadySocket {
                    socket: *socket,
                    writable: false,
                });
            }
        }

        Ok(ready.len())
    }
}

impl<O> Drop for SrtEgressPoller<O>
where
    O: SrtPollOps,
{
    fn drop(&mut self) {
        self.ops.release(self.eid);
    }
}

fn events_for(interest: SrtEgressInterest) -> c_int {
    let mut events = SRT_EPOLL_ERR;
    if interest.writable {
        events |= SRT_EPOLL_OUT;
    }
    events
}

pub(super) trait SrtPollOps {
    fn create(&self) -> c_int;
    fn add_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int;
    fn update_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int;
    fn remove_usock(&self, eid: c_int, socket: SRTSOCKET) -> c_int;
    fn wait(
        &self,
        eid: c_int,
        readfds: &mut [SRTSOCKET],
        read_count: &mut c_int,
        writefds: &mut [SRTSOCKET],
        write_count: &mut c_int,
        timeout_ms: i64,
    ) -> c_int;
    fn release(&self, eid: c_int) -> c_int;
    fn error(&self, operation: &'static str) -> SrtEgressPollError;
}

pub(super) struct LibSrtPollOps;

impl SrtPollOps for LibSrtPollOps {
    fn create(&self) -> c_int {
        // SAFETY: creates an independent libsrt epoll container and returns
        // its handle or a negative error code.
        unsafe { srt_epoll_create() }
    }

    fn add_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int {
        // SAFETY: `eid` and `socket` are live libsrt handles owned by the
        // caller. `events` is passed by address for the duration of the call.
        unsafe { srt_epoll_add_usock(eid, socket, &events) }
    }

    fn update_usock(&self, eid: c_int, socket: SRTSOCKET, events: c_int) -> c_int {
        // SAFETY: same handle and event-pointer contract as `add_usock`.
        unsafe { srt_epoll_update_usock(eid, socket, &events) }
    }

    fn remove_usock(&self, eid: c_int, socket: SRTSOCKET) -> c_int {
        // SAFETY: removes `socket` from the live epoll container `eid`.
        unsafe { srt_epoll_remove_usock(eid, socket) }
    }

    fn wait(
        &self,
        eid: c_int,
        readfds: &mut [SRTSOCKET],
        read_count: &mut c_int,
        writefds: &mut [SRTSOCKET],
        write_count: &mut c_int,
        timeout_ms: i64,
    ) -> c_int {
        // SAFETY: both buffers are valid for their advertised lengths, and the
        // count pointers remain valid for the duration of the blocking wait.
        unsafe {
            srt_epoll_wait(
                eid,
                readfds.as_mut_ptr(),
                read_count,
                writefds.as_mut_ptr(),
                write_count,
                timeout_ms,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    }

    fn release(&self, eid: c_int) -> c_int {
        // SAFETY: releases the epoll container; the wrapper calls this once
        // from Drop for each successful create.
        unsafe { srt_epoll_release(eid) }
    }

    fn error(&self, operation: &'static str) -> SrtEgressPollError {
        let (code, message) = last_srt_error();
        SrtEgressPollError::new(operation, code, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
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
            SrtEgressPollError::new(operation, 1234, "fake error".to_string())
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
}
