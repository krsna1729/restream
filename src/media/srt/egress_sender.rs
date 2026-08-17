use bytes::Bytes;
use std::os::raw::c_int;

use crate::media::egress::backend::{CloseReason, Readiness};
use crate::media::srt::srt_egress_poller::SrtEgressInterest;

use super::socket::last_srt_error;
use super::sys::{
    SRT_EASYNCSND, SRT_ECONNLOST, SRT_ENOCONN, SRT_ESCLOSED, SRTSOCKET, SrtTraceBStats,
    srt_bistats, srt_close, srt_send,
};

/// Native libsrt sender-buffer occupancy for one socket.
///
/// The fabric charges these bytes against the leaf's memory envelope in
/// addition to the engine's retained application message: a leaf whose
/// application queue is drained but whose native buffer is saturated is
/// backpressured, not idle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NativeSendBacklog {
    pub bytes: u64,
    pub packets: u32,
    pub ms: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SrtSenderStats {
    pub packets_sent_loss_total: u64,
    pub packets_sent_drop_total: u64,
    pub packets_retransmit_total: u64,
    pub packets_received_nak_total: u64,
    pub rtt_ms: f64,
    pub send_rate_mbps: f64,
    pub bandwidth_mbps: f64,
    pub send_tsbpd_delay_ms: f64,
    pub send_buf_ms: f64,
    pub send_buf_bytes: i32,
    pub send_buf_available_bytes: i32,
    pub flight_size_packets: i32,
    pub flow_window_packets: i32,
    pub congestion_window_packets: i32,
}

impl From<SrtTraceBStats> for SrtSenderStats {
    fn from(stats: SrtTraceBStats) -> Self {
        Self {
            packets_sent_loss_total: stats.pkt_snd_loss_total.max(0) as u64,
            packets_sent_drop_total: stats.pkt_snd_drop_total.max(0) as u64,
            packets_retransmit_total: stats.pkt_retrans_total.max(0) as u64,
            packets_received_nak_total: stats.pkt_recv_nak_total.max(0) as u64,
            rtt_ms: stats.ms_rtt,
            send_rate_mbps: stats.mbps_send_rate,
            bandwidth_mbps: stats.mbps_bandwidth,
            send_tsbpd_delay_ms: stats.ms_snd_tsb_pd_delay.max(0) as f64,
            send_buf_ms: stats.ms_snd_buf.max(0) as f64,
            send_buf_bytes: stats.byte_snd_buf.max(0),
            send_buf_available_bytes: stats.byte_avail_snd_buf.max(0),
            flight_size_packets: stats.pkt_flight_size.max(0),
            flow_window_packets: stats.pkt_flow_window.max(0),
            congestion_window_packets: stats.pkt_congestion_window.max(0),
        }
    }
}

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

    fn on_readiness(&mut self, _readiness: Readiness) {}

    fn readiness_interest(&self) -> SrtEgressInterest {
        SrtEgressInterest::WRITE
    }

    fn dynamic_readiness(&self) -> bool {
        false
    }

    /// Instantaneous native sender-buffer occupancy, when the transport has
    /// one.  `None` means the transport exposes no native buffer (fakes,
    /// closed sockets) and only application pending state applies.
    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        None
    }

    /// Raw libsrt sender-side statistics for connection-quality reporting
    /// (RTT, loss, retransmits, bandwidth — status `quality` source).
    /// `None` means the transport exposes no native stats (fakes, closed
    /// sockets); the caller keeps the previous quality snapshot in that
    /// case rather than overwriting it with an empty one.
    fn sender_quality_stats(&self) -> Option<SrtSenderStats> {
        None
    }
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

    fn on_readiness(&mut self, readiness: Readiness) {
        (**self).on_readiness(readiness);
    }

    fn readiness_interest(&self) -> SrtEgressInterest {
        (**self).readiness_interest()
    }

    fn dynamic_readiness(&self) -> bool {
        (**self).dynamic_readiness()
    }

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        (**self).native_send_backlog()
    }

    fn sender_quality_stats(&self) -> Option<SrtSenderStats> {
        (**self).sender_quality_stats()
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

    #[cfg(test)]
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
            tracing::warn!("srt send peer closed: transport socket already closed (None)");
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

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        self.socket.and_then(|socket| self.ops.send_backlog(socket))
    }

    fn sender_quality_stats(&self) -> Option<SrtSenderStats> {
        self.socket
            .and_then(|socket| self.ops.sender_quality_stats(socket))
            .map(Into::into)
    }
}

pub(super) trait SrtSendOps {
    fn send(&self, socket: SRTSOCKET, message: &Bytes) -> c_int;
    fn close(&self, socket: SRTSOCKET) -> c_int;
    fn error(&self) -> (c_int, String);

    /// Instantaneous send-buffer occupancy; `None` when unavailable.
    fn send_backlog(&self, socket: SRTSOCKET) -> Option<NativeSendBacklog>;

    /// Raw libsrt sender-side statistics; `None` when unavailable.
    fn sender_quality_stats(&self, socket: SRTSOCKET) -> Option<SrtTraceBStats>;
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

    fn send_backlog(&self, socket: SRTSOCKET) -> Option<NativeSendBacklog> {
        // SAFETY: Category 8 - FFI boundary. `SrtTraceBStats` is a repr(C)
        // plain-data struct, so the zeroed value is valid; `socket` is a live
        // libsrt socket owned by this sender and libsrt fills the struct.
        // clear=0 keeps counters intact; instantaneous=1 asks only for
        // current buffer occupancy without a full stats sweep.
        let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
        let result = unsafe { srt_bistats(socket, &mut stats, 0, 1) };
        if result < 0 {
            return None;
        }
        Some(NativeSendBacklog {
            bytes: stats.byte_snd_buf.max(0) as u64,
            packets: stats.pkt_snd_buf.max(0) as u32,
            ms: stats.ms_snd_buf.max(0) as u32,
        })
    }

    fn sender_quality_stats(&self, socket: SRTSOCKET) -> Option<SrtTraceBStats> {
        // SAFETY: Category 8 - FFI boundary. Same call shape as
        // `send_backlog` above: `socket` is a live libsrt socket owned by
        // this sender, `stats` is a repr(C) plain-data struct valid when
        // zeroed, and clear=0/instantaneous=1 matches the flags legacy SRT
        // egress used for its own quality sampling.
        let mut stats: SrtTraceBStats = unsafe { std::mem::zeroed() };
        let result = unsafe { srt_bistats(socket, &mut stats, 0, 1) };
        (result >= 0).then_some(stats)
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
        SRT_ESCLOSED | SRT_ECONNLOST | SRT_ENOCONN => {
            tracing::warn!(code, "srt send peer closed: {message} ({code})");
            SrtSendResult::PeerClosed
        }
        _ => SrtSendResult::Failed(SrtSendFailure {
            reason: "srt_send",
            detail: format!("{message} ({code})"),
            retryable: true,
        }),
    }
}
