use std::net::SocketAddr;

use shiguredo_srt::GroupExtensionData;

use super::connection::RustConnection;

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
        group: Option<GroupExtensionData>,
        peer_socket_id: u32,
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

pub(super) enum WorkerCommand {
    Authorize {
        id: ConnectionId,
        logical_id: ConnectionId,
        accepted: bool,
    },
    Handoff {
        connection: Box<RustConnection>,
    },
    ForwardPacket {
        peer: SocketAddr,
        packet: Vec<u8>,
    },
    Send {
        id: ConnectionId,
        payload: Vec<u8>,
    },
    Close {
        id: ConnectionId,
        reason: String,
    },
    Stop,
}
