use bytes::Bytes;

use super::srt_egress_sender::*;
use super::sys::{SRT_EASYNCSND, SRT_ECONNLOST, SRT_ENOCONN, SRT_ESCLOSED, SrtTraceBStats};
use crate::media::egress::backend::CloseReason;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::rc::Rc;

#[derive(Clone)]
struct FakeSendOps {
    sends: Rc<RefCell<Vec<(i32, Bytes)>>>,
    closes: Rc<RefCell<Vec<i32>>>,
    results: Rc<RefCell<VecDeque<c_int>>>,
    errors: Rc<RefCell<VecDeque<(c_int, String)>>>,
    backlog: Rc<Cell<Option<NativeSendBacklog>>>,
}

impl FakeSendOps {
    fn with_send_result(result: c_int) -> Self {
        let ops = Self::default();
        ops.results.borrow_mut().push_back(result);
        ops
    }

    fn with_error(code: c_int, message: &str) -> Self {
        let ops = Self::with_send_result(-1);
        ops.errors
            .borrow_mut()
            .push_back((code, message.to_string()));
        ops
    }
}

impl Default for FakeSendOps {
    fn default() -> Self {
        Self {
            sends: Rc::new(RefCell::new(Vec::new())),
            closes: Rc::new(RefCell::new(Vec::new())),
            results: Rc::new(RefCell::new(VecDeque::new())),
            errors: Rc::new(RefCell::new(VecDeque::new())),
            backlog: Rc::new(Cell::new(None)),
        }
    }
}

impl SrtSendOps for FakeSendOps {
    fn send(&self, socket: i32, message: &Bytes) -> c_int {
        self.sends.borrow_mut().push((socket, message.clone()));
        self.results
            .borrow_mut()
            .pop_front()
            .unwrap_or(message.len() as c_int)
    }

    fn close(&self, socket: i32) -> c_int {
        self.closes.borrow_mut().push(socket);
        0
    }

    fn error(&self) -> (c_int, String) {
        self.errors
            .borrow_mut()
            .pop_front()
            .unwrap_or((-1, "native send failed".to_string()))
    }

    fn send_backlog(&self, _socket: i32) -> Option<NativeSendBacklog> {
        self.backlog.get()
    }

    fn sender_quality_stats(&self, _socket: i32) -> Option<SrtTraceBStats> {
        None
    }
}

#[test]
fn native_sender_reports_native_backlog_while_socket_is_open() {
    let ops = FakeSendOps::default();
    ops.backlog.set(Some(NativeSendBacklog {
        bytes: 8_192,
        packets: 6,
        ms: 120,
    }));
    let mut sender = SrtNativeMessageSender::with_ops(7, ops.clone());

    assert_eq!(
        sender.native_send_backlog(),
        Some(NativeSendBacklog {
            bytes: 8_192,
            packets: 6,
            ms: 120,
        })
    );

    // After close the socket is gone: no native buffer to account.
    sender.close(CloseReason::Removed);
    assert_eq!(sender.native_send_backlog(), None);
}

#[test]
fn native_sender_reports_accepted_bytes_when_srt_send_succeeds() {
    let ops = FakeSendOps::with_send_result(3);
    let mut sender = SrtNativeMessageSender::with_ops(42, ops.clone());

    let result = sender.send_message(&Bytes::from_static(b"abc"));

    assert_eq!(result, SrtSendResult::Accepted { bytes: 3 });
    assert_eq!(
        ops.sends.borrow().as_slice(),
        &[(42, Bytes::from_static(b"abc"))]
    );
}

#[test]
fn native_sender_maps_async_send_backpressure_to_would_block() {
    let ops = FakeSendOps::with_error(SRT_EASYNCSND, "async send would block");
    let mut sender = SrtNativeMessageSender::with_ops(42, ops);

    let result = sender.send_message(&Bytes::from_static(b"abc"));

    assert_eq!(result, SrtSendResult::WouldBlock);
}

#[test]
fn native_sender_maps_disconnect_errors_to_peer_closed() {
    for code in [SRT_ESCLOSED, SRT_ECONNLOST, SRT_ENOCONN] {
        let ops = FakeSendOps::with_error(code, "connection closed");
        let mut sender = SrtNativeMessageSender::with_ops(42, ops);

        let result = sender.send_message(&Bytes::from_static(b"abc"));

        assert_eq!(result, SrtSendResult::PeerClosed);
    }
}

#[test]
fn native_sender_maps_other_errors_to_retryable_send_failure() {
    let ops = FakeSendOps::with_error(7000, "packet rejected");
    let mut sender = SrtNativeMessageSender::with_ops(42, ops);

    let result = sender.send_message(&Bytes::from_static(b"abc"));

    assert_eq!(
        result,
        SrtSendResult::Failed(SrtSendFailure {
            reason: "srt_send",
            detail: "packet rejected (7000)".to_string(),
            retryable: true,
        })
    );
}

#[test]
fn native_sender_close_closes_socket_once_and_marks_sender_closed() {
    let ops = FakeSendOps::default();
    let mut sender = SrtNativeMessageSender::with_ops(42, ops.clone());

    sender.close(CloseReason::Removed);
    sender.close(CloseReason::Removed);
    let result = sender.send_message(&Bytes::from_static(b"abc"));

    assert_eq!(ops.closes.borrow().as_slice(), &[42]);
    assert_eq!(sender.socket(), None);
    assert_eq!(result, SrtSendResult::PeerClosed);
}
