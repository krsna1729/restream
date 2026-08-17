use crate::media::srt::{
    SrtEgressSendMode, SrtEgressSocketError, SrtLeafHandle, configure_connected_srt_egress_socket,
};

pub(crate) trait SrtSocketConfigurator {
    fn configure_connected(
        &mut self,
        handle: SrtLeafHandle,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError>;
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtSocketConfigurator;

impl SrtSocketConfigurator for NativeSrtSocketConfigurator {
    fn configure_connected(
        &mut self,
        handle: SrtLeafHandle,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        let SrtLeafHandle::Native(socket) = handle else {
            return Err(SrtEgressSocketError {
                option: "SRTSOCKET",
                code: -1,
                message: "native SRT configurator cannot configure a Rust transport handle"
                    .to_string(),
            });
        };
        configure_connected_srt_egress_socket(socket, mode)
    }
}
