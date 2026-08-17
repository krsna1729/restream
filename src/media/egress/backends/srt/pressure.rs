use super::{NativeSendBacklog, SrtLeafHandle};
use crate::media::egress::scheduler::LeafKey;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtLeafPressure {
    pub app_pending_bytes: usize,
    pub native_backlog: Option<NativeSendBacklog>,
}

impl SrtLeafPressure {
    pub(crate) fn pending_bytes(&self) -> u64 {
        self.app_pending_bytes as u64 + self.native_backlog.map_or(0, |backlog| backlog.bytes)
    }

    pub(crate) fn is_backpressured(&self) -> bool {
        self.pending_bytes() > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SrtLeafSocket {
    pub(crate) key: LeafKey,
    pub(crate) handle: SrtLeafHandle,
}
