use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ConnectionId {
    pub(super) worker: usize,
    pub(super) serial: u64,
}

#[derive(Debug)]
pub(super) enum IngestEvent {
    Connected {
        id: ConnectionId,
        peer: SocketAddr,
        stream_id: String,
    },
    Data {
        id: ConnectionId,
        payload: Vec<u8>,
    },
    Disconnected {
        id: ConnectionId,
        phase: &'static str,
        reason: String,
        had_error: bool,
    },
}

#[derive(Debug)]
pub(super) enum WorkerCommand {
    Authorize { id: ConnectionId, accepted: bool },
    Stop,
}
