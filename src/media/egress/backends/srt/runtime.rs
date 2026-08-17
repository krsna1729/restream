use super::{
    SrtEgressInterest, SrtEgressPollError, SrtEgressSendMode, SrtFabricEgressConnectConfig,
    SrtFabricPoller, SrtLeafHandle, SrtMessageSender, SrtReadinessPoller, SrtReadyLeaf,
    SrtResolveCompletionSource, SrtResolvedConnect, SrtSocketConfigurator, SrtSocketConnector,
};
use crate::media::egress::scheduler::LeafKey;
use crate::media::srt::{
    SrtEgressSocketError, connect_fabric_srt_egress_socket, srt_fabric_message_sender,
    srt_get_configured_sndbuf,
};

use super::socket_config::NativeSrtSocketConfigurator;

pub(crate) struct SrtConnectedTransport {
    pub(crate) handle: SrtLeafHandle,
    pub(crate) sender: Box<dyn SrtMessageSender + Send>,
    pub(crate) configured_sndbuf: Option<i32>,
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtSocketConnector;

impl SrtSocketConnector for NativeSrtSocketConnector {
    fn connect(
        &mut self,
        config: SrtFabricEgressConnectConfig<'_>,
    ) -> Result<SrtConnectedTransport, String> {
        let socket = connect_fabric_srt_egress_socket(config)?;
        Ok(SrtConnectedTransport {
            handle: SrtLeafHandle::Native(socket),
            sender: srt_fabric_message_sender(socket),
            configured_sndbuf: Some(srt_get_configured_sndbuf(socket)),
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct SrtRustSocketConnector;

impl SrtSocketConnector for SrtRustSocketConnector {
    fn connect(
        &mut self,
        config: SrtFabricEgressConnectConfig<'_>,
    ) -> Result<SrtConnectedTransport, String> {
        super::rs_sender::connected_transport(config)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SrtRustSocketConfigurator;

impl SrtSocketConfigurator for SrtRustSocketConfigurator {
    fn configure_connected(
        &mut self,
        handle: SrtLeafHandle,
        _mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        match handle {
            SrtLeafHandle::Rust(_) => Ok(()),
            SrtLeafHandle::Native(_) => Err(SrtEgressSocketError {
                option: "SRTSOCKET",
                code: -1,
                message: "Rust SRT configurator received a native handle".to_string(),
            }),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct NoopSrtResolveCompletionSource;

impl SrtResolveCompletionSource for NoopSrtResolveCompletionSource {
    fn drain_resolved(&mut self, _resolved: &mut Vec<SrtResolvedConnect>) {}
}

impl SrtReadinessPoller for SrtFabricPoller {
    fn register_leaf(
        &mut self,
        handle: SrtLeafHandle,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.register_leaf(handle, key, generation, interest)
    }

    fn remove(&mut self, handle: SrtLeafHandle) -> Result<(), SrtEgressPollError> {
        self.remove(handle)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

impl SrtReadinessPoller for super::SrtRustFabricPoller {
    fn register_leaf(
        &mut self,
        handle: SrtLeafHandle,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.register(handle, key, generation, interest)
    }

    fn remove(&mut self, handle: SrtLeafHandle) -> Result<(), SrtEgressPollError> {
        self.remove(handle)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

pub(crate) enum SrtRuntimePoller {
    Native(SrtFabricPoller),
    Rust(super::SrtRustFabricPoller),
}

impl SrtRuntimePoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, SrtEgressPollError> {
        if crate::config::rust_srt_backend_selected() {
            super::SrtRustFabricPoller::new(max_events).map(Self::Rust)
        } else {
            SrtFabricPoller::new(max_events).map(Self::Native)
        }
    }
}

impl SrtReadinessPoller for SrtRuntimePoller {
    fn register_leaf(
        &mut self,
        handle: SrtLeafHandle,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        match self {
            Self::Native(poller) => poller.register_leaf(handle, key, generation, interest),
            Self::Rust(poller) => poller.register_leaf(handle, key, generation, interest),
        }
    }

    fn remove(&mut self, handle: SrtLeafHandle) -> Result<(), SrtEgressPollError> {
        match self {
            Self::Native(poller) => poller.remove(handle),
            Self::Rust(poller) => poller.remove(handle),
        }
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        match self {
            Self::Native(poller) => poller.poll_leaves(timeout_ms, ready),
            Self::Rust(poller) => poller.poll_leaves(timeout_ms, ready),
        }
    }
}

pub(crate) enum SrtRuntimeSocketConfigurator {
    Native(NativeSrtSocketConfigurator),
    Rust(SrtRustSocketConfigurator),
}

impl SrtRuntimeSocketConfigurator {
    pub(crate) fn from_environment() -> Self {
        if crate::config::rust_srt_backend_selected() {
            Self::Rust(SrtRustSocketConfigurator)
        } else {
            Self::Native(NativeSrtSocketConfigurator)
        }
    }
}

impl SrtSocketConfigurator for SrtRuntimeSocketConfigurator {
    fn configure_connected(
        &mut self,
        handle: SrtLeafHandle,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        match self {
            Self::Native(configurator) => configurator.configure_connected(handle, mode),
            Self::Rust(configurator) => configurator.configure_connected(handle, mode),
        }
    }
}

pub(crate) enum SrtRuntimeSocketConnector {
    Native(NativeSrtSocketConnector),
    Rust(SrtRustSocketConnector),
}

impl SrtRuntimeSocketConnector {
    pub(crate) fn from_environment() -> Self {
        if crate::config::rust_srt_backend_selected() {
            Self::Rust(SrtRustSocketConnector)
        } else {
            Self::Native(NativeSrtSocketConnector)
        }
    }
}

impl SrtSocketConnector for SrtRuntimeSocketConnector {
    fn connect(
        &mut self,
        config: SrtFabricEgressConnectConfig<'_>,
    ) -> Result<SrtConnectedTransport, String> {
        match self {
            Self::Native(connector) => connector.connect(config),
            Self::Rust(connector) => connector.connect(config),
        }
    }
}
