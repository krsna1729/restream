#![allow(dead_code)]

use bytes::Bytes;
use std::os::raw::c_int;

use crate::media::egress::backend::CloseReason;

use super::socket::last_srt_error;
use super::sys::{
    SRT_EASYNCSND, SRT_ECONNLOST, SRT_ENOCONN, SRT_ESCLOSED, SRTSOCKET, srt_close, srt_send,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SrtSendFailure {
    pub reason: &'static str,
    pub detail: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtSendResult {
    Accepted { bytes: usize },
    WouldBlock,
    PeerClosed,
    Failed(SrtSendFailure),
}

pub(crate) trait SrtMessageSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult;
    fn close(&mut self, reason: CloseReason);
}

impl<T> SrtMessageSender for Box<T>
where
    T: SrtMessageSender + ?Sized,
{
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        (**self).send_message(message)
    }

    fn close(&mut self, reason: CloseReason) {
        (**self).close(reason);
    }
}

#[derive(Debug)]
pub(super) struct SrtNativeMessageSender<O = LibSrtSendOps>
where
    O: SrtSendOps,
{
    socket: Option<SRTSOCKET>,
    ops: O,
}

impl SrtNativeMessageSender<LibSrtSendOps> {
    pub(super) fn new(socket: SRTSOCKET) -> Self {
        Self::with_ops(socket, LibSrtSendOps)
    }
}

impl<O> SrtNativeMessageSender<O>
where
    O: SrtSendOps,
{
    pub(super) fn with_ops(socket: SRTSOCKET, ops: O) -> Self {
        Self {
            socket: Some(socket),
            ops,
        }
    }

    pub(super) fn socket(&self) -> Option<SRTSOCKET> {
        self.socket
    }
}

impl<O> SrtMessageSender for SrtNativeMessageSender<O>
where
    O: SrtSendOps,
{
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        let Some(socket) = self.socket else {
            return SrtSendResult::PeerClosed;
        };

        let result = self.ops.send(socket, message);
        classify_srt_send_result(result, || self.ops.error())
    }

    fn close(&mut self, _reason: CloseReason) {
        if let Some(socket) = self.socket.take() {
            let _ = self.ops.close(socket);
        }
    }
}

pub(super) trait SrtSendOps {
    fn send(&self, socket: SRTSOCKET, message: &Bytes) -> c_int;
    fn close(&self, socket: SRTSOCKET) -> c_int;
    fn error(&self) -> (c_int, String);
}

#[derive(Debug)]
pub(super) struct LibSrtSendOps;

impl SrtSendOps for LibSrtSendOps {
    fn send(&self, socket: SRTSOCKET, message: &Bytes) -> c_int {
        // SAFETY: Category 8 - FFI boundary. `socket` is a live connected
        // libsrt socket owned by this sender, and `message.as_ptr()` is valid
        // for `message.len()` bytes for the duration of the call.
        unsafe { srt_send(socket, message.as_ptr(), message.len() as c_int) }
    }

    fn close(&self, socket: SRTSOCKET) -> c_int {
        // SAFETY: Category 8 - FFI boundary. The sender takes each socket out
        // of `Option` before closing, so this wrapper closes the handle at most
        // once after ownership has moved to the native sender.
        unsafe { srt_close(socket) }
    }

    fn error(&self) -> (c_int, String) {
        last_srt_error()
    }
}

fn classify_srt_send_result(
    result: c_int,
    error: impl FnOnce() -> (c_int, String),
) -> SrtSendResult {
    if result >= 0 {
        return SrtSendResult::Accepted {
            bytes: result as usize,
        };
    }

    let (code, message) = error();
    match code {
        SRT_EASYNCSND => SrtSendResult::WouldBlock,
        SRT_ESCLOSED | SRT_ECONNLOST | SRT_ENOCONN => SrtSendResult::PeerClosed,
        _ => SrtSendResult::Failed(SrtSendFailure {
            reason: "srt_send",
            detail: format!("{message} ({code})"),
            retryable: true,
        }),
    }
}
