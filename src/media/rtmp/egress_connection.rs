use std::io;
use std::time::Duration;

use bytes::Bytes;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use tokio_util::sync::CancellationToken;
use tracing::error;

use super::egress_transport::{RtmpEgressStream, RtmpUrlParts, connect_rtmp_egress_stream};
use super::egress_write::{RtmpWriteQueue, write_rtmp_pending_bytes};
use super::enhanced::enhanced_rtmp_connect_packet;
use super::handshake::perform_client_handshake;

const RTMP_EGRESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct RtmpEgressConnection {
    pub(super) parts: RtmpUrlParts,
    pub(super) socket: RtmpEgressStream,
    pub(super) remaining: Vec<u8>,
}

pub(super) struct RtmpEgressSession {
    parts: RtmpUrlParts,
    socket: RtmpEgressStream,
    remaining: Vec<u8>,
    session: ClientSession,
    connect_config: ClientSessionConfig,
    initial_results: Vec<ClientSessionResult>,
    write_queue: RtmpWriteQueue,
}

pub(super) enum InitialServerResultError {
    Parse,
    Dispatch,
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

    pub(super) fn initialize_client_session(
        self,
        chunk_size: u32,
    ) -> Result<RtmpEgressSession, String> {
        let mut config = ClientSessionConfig::new();
        let scheme = if self.parts.tls { "rtmps" } else { "rtmp" };
        config.tc_url = Some(format!(
            "{}://{}:{}/{}",
            scheme, self.parts.host, self.parts.port, self.parts.app
        ));
        config.chunk_size = chunk_size;
        let connect_config = config.clone();
        let (session, initial_results) =
            ClientSession::new(config).map_err(|error| format!("{error:?}"))?;

        Ok(RtmpEgressSession {
            parts: self.parts,
            socket: self.socket,
            remaining: self.remaining,
            session,
            connect_config,
            initial_results,
            write_queue: RtmpWriteQueue::default(),
        })
    }
}

impl RtmpEgressSession {
    pub(super) async fn write_initial_results(&mut self) -> io::Result<()> {
        for result in self.initial_results.drain(..) {
            if let ClientSessionResult::OutboundResponse(packet) = result {
                write_rtmp_pending_bytes(
                    &mut self.socket,
                    &mut self.write_queue,
                    Bytes::from(packet.bytes),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn request_connection(&mut self, enhanced: bool) -> Result<(), String> {
        let packet = match self.session.request_connection(self.parts.app.clone()) {
            Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
            _ => return Err("failed to build connect request".to_string()),
        };
        let bytes = if enhanced {
            enhanced_rtmp_connect_packet(&self.connect_config, &self.parts.app)?
        } else {
            packet.bytes
        };
        write_rtmp_pending_bytes(&mut self.socket, &mut self.write_queue, Bytes::from(bytes))
            .await
            .map_err(|_| "failed to write connect request".to_string())?;
        Ok(())
    }

    pub(super) async fn handle_initial_server_results(
        &mut self,
    ) -> Result<(), InitialServerResultError> {
        if self.remaining.is_empty() {
            return Ok(());
        }
        let results = self
            .session
            .handle_input(&self.remaining)
            .map_err(|_| InitialServerResultError::Parse)?;
        self.handle_server_results(results)
            .await
            .map_err(|_| InitialServerResultError::Dispatch)
    }

    async fn handle_server_results(
        &mut self,
        results: Vec<ClientSessionResult>,
    ) -> Result<(), &'static str> {
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    write_rtmp_pending_bytes(
                        &mut self.socket,
                        &mut self.write_queue,
                        Bytes::from(packet.bytes),
                    )
                    .await
                    .map_err(|_| "Socket write error")?;
                }
                ClientSessionResult::RaisedEvent(event) => match event {
                    ClientSessionEvent::ConnectionRequestAccepted => {
                        let packet = match self.session.request_publishing(
                            self.parts.stream_key.clone(),
                            PublishRequestType::Live,
                        ) {
                            Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
                            _ => return Err("Failed to build publish request"),
                        };
                        write_rtmp_pending_bytes(
                            &mut self.socket,
                            &mut self.write_queue,
                            Bytes::from(packet.bytes),
                        )
                        .await
                        .map_err(|_| "Socket write error")?;
                    }
                    ClientSessionEvent::ConnectionRequestRejected { description } => {
                        error!("Connection request rejected: {}", description);
                        return Err("Connection request rejected");
                    }
                    _ => {}
                },
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        Ok(())
    }

    pub(super) fn into_legacy_parts(
        self,
    ) -> (
        RtmpUrlParts,
        RtmpEgressStream,
        ClientSession,
        RtmpWriteQueue,
    ) {
        (self.parts, self.socket, self.session, self.write_queue)
    }
}
