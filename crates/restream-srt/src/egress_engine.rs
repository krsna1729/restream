//! SRT egress constants and send outcomes.

pub const MAX_SRT_MESSAGE_PAYLOAD: usize = 1316;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrtSendResult {
    Accepted {
        bytes: usize,
    },
    /// The caller cannot take payload right now (still connecting, or its
    /// send window is full).
    WouldBlock,
    PeerClosed,
    Failed {
        reason: &'static str,
        detail: String,
        retryable: bool,
    },
}
