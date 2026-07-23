use std::io;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::egress_transport::{RtmpEgressStream, RtmpUrlParts, connect_rtmp_egress_stream};
use super::handshake::perform_client_handshake;

const RTMP_EGRESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct RtmpEgressConnection {
    pub(super) parts: RtmpUrlParts,
    pub(super) socket: RtmpEgressStream,
    pub(super) remaining: Vec<u8>,
}

impl RtmpEgressConnection {
    pub(super) async fn connect(parts: RtmpUrlParts, buffer_size: usize) -> io::Result<Self> {
        let socket = connect_rtmp_egress_stream(&parts, buffer_size).await?;
        Ok(Self {
            parts,
            socket,
            remaining: Vec::new(),
        })
    }

    pub(super) async fn perform_handshake(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        self.remaining = tokio::time::timeout(
            RTMP_EGRESS_HANDSHAKE_TIMEOUT,
            perform_client_handshake(&mut self.socket, cancel),
        )
        .await
        .map_err(|_| "RTMP egress handshake timed out".to_string())??;
        Ok(())
    }
}
