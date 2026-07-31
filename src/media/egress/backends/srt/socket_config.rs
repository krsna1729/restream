use crate::media::srt::{
    SRTSOCKET, SrtEgressSendMode, SrtEgressSocketError, configure_connected_srt_egress_socket,
};

pub(crate) trait SrtSocketConfigurator {
    fn configure_connected(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError>;
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtSocketConfigurator;

impl SrtSocketConfigurator for NativeSrtSocketConfigurator {
    fn configure_connected(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        configure_connected_srt_egress_socket(socket, mode)
    }
}
