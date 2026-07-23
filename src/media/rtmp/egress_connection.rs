use std::io;
use std::time::Duration;

use bytes::Bytes;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::egress_transport::{
    RtmpEgressStream, RtmpUrlParts, connect_rtmp_egress_stream, rtmp_sender_quality,
};
use super::egress_write::{RtmpWriteQueue, RtmpWriteQueueError, flush_rtmp_pending_bytes};
use super::enhanced::enhanced_rtmp_connect_packet;
use super::handshake::perform_client_handshake;
use crate::media::snapshots::PublisherQuality;

const RTMP_EGRESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct RtmpEgressConnection {
    pub(super) parts: RtmpUrlParts,
    pub(super) socket: RtmpEgressStream,
    pub(super) remaining: Vec<u8>,
}

pub(super) struct RtmpEgressSession {
    socket: RtmpEgressStream,
    remaining: Vec<u8>,
    core: RtmpSessionCore,
}

struct RtmpSessionCore {
    parts: RtmpUrlParts,
    session: ClientSession,
    connect_config: ClientSessionConfig,
    initial_results: Vec<ClientSessionResult>,
    write_queue: RtmpWriteQueue,
}

pub(super) enum InitialServerResultError {
    Parse,
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RtmpSessionEvent {
    ConnectionRequestAccepted,
    PublishRequestAccepted,
}

#[derive(Debug)]
pub(super) enum RtmpSessionError {
    Protocol(&'static str),
    Pending(RtmpWriteQueueError),
    Socket(io::Error),
    ConnectionRejected(String),
}

impl std::fmt::Display for RtmpSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(detail) => detail.fmt(formatter),
            Self::Pending(error) => write!(formatter, "RTMP pending write rejected: {error:?}"),
            Self::Socket(error) => error.fmt(formatter),
            Self::ConnectionRejected(description) => {
                write!(formatter, "connection request rejected: {description}")
            }
        }
    }
}

impl std::error::Error for RtmpSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(_) | Self::Pending(_) | Self::ConnectionRejected(_) => None,
            Self::Socket(error) => Some(error),
        }
    }
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
        max_pending_bytes: usize,
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
            socket: self.socket,
            remaining: self.remaining,
            core: RtmpSessionCore {
                parts: self.parts,
                session,
                connect_config,
                initial_results,
                write_queue: RtmpWriteQueue::new(max_pending_bytes),
            },
        })
    }
}

impl RtmpEgressSession {
    pub(super) async fn write_initial_results(&mut self) -> io::Result<()> {
        self.core
            .queue_initial_results()
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.flush_pending()
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    }

    pub(super) async fn request_connection(&mut self, enhanced: bool) -> Result<(), String> {
        self.core.request_connection(enhanced)?;
        self.flush_pending()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) async fn handle_initial_server_results(
        &mut self,
    ) -> Result<(), InitialServerResultError> {
        if self.remaining.is_empty() {
            return Ok(());
        }
        let remaining = std::mem::take(&mut self.remaining);
        self.core
            .handle_server_input(&remaining)
            .map_err(|error| match error {
                RtmpSessionError::Protocol(_) => InitialServerResultError::Parse,
                RtmpSessionError::Pending(_)
                | RtmpSessionError::Socket(_)
                | RtmpSessionError::ConnectionRejected(_) => InitialServerResultError::Dispatch,
            })?;
        self.flush_pending()
            .await
            .map(|_| ())
            .map_err(|error| match error {
                RtmpSessionError::Protocol(_) => InitialServerResultError::Parse,
                RtmpSessionError::Pending(_)
                | RtmpSessionError::Socket(_)
                | RtmpSessionError::ConnectionRejected(_) => InitialServerResultError::Dispatch,
            })
    }

    pub(super) async fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.socket.read(buffer).await
    }

    pub(super) fn sender_quality(
        &self,
        previous_tcp_bytes: &mut Option<(u64, std::time::Instant)>,
    ) -> PublisherQuality {
        rtmp_sender_quality(&self.socket, previous_tcp_bytes)
    }

    pub(super) fn stop_publishing(&mut self) {
        self.core.stop_publishing();
    }

    pub(super) async fn handle_server_input(
        &mut self,
        input: &[u8],
    ) -> Result<Vec<RtmpSessionEvent>, RtmpSessionError> {
        let events = self.core.handle_server_input(input)?;
        self.flush_pending().await?;
        Ok(events)
    }

    pub(super) async fn publish_metadata(
        &mut self,
        metadata: &StreamMetadata,
    ) -> Result<(), RtmpSessionError> {
        self.core.publish_metadata(metadata)?;
        self.flush_pending().await
    }

    pub(super) async fn publish_video_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let bytes = self
            .core
            .publish_video_data(payload, timestamp, can_be_dropped)?;
        self.flush_pending().await?;
        Ok(bytes)
    }

    pub(super) async fn publish_audio_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let bytes = self
            .core
            .publish_audio_data(payload, timestamp, can_be_dropped)?;
        self.flush_pending().await?;
        Ok(bytes)
    }

    async fn flush_pending(&mut self) -> Result<(), RtmpSessionError> {
        flush_rtmp_pending_bytes(&mut self.socket, &mut self.core.write_queue)
            .await
            .map_err(RtmpSessionError::Socket)
    }
}

impl RtmpSessionCore {
    fn queue_initial_results(&mut self) -> Result<(), RtmpSessionError> {
        let initial_results = std::mem::take(&mut self.initial_results);
        for result in initial_results {
            if let ClientSessionResult::OutboundResponse(packet) = result {
                self.queue_packet(Bytes::from(packet.bytes))?;
            }
        }
        Ok(())
    }

    fn request_connection(&mut self, enhanced: bool) -> Result<(), String> {
        let packet = match self.session.request_connection(self.parts.app.clone()) {
            Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
            _ => return Err("failed to build connect request".to_string()),
        };
        let bytes = if enhanced {
            enhanced_rtmp_connect_packet(&self.connect_config, &self.parts.app)?
        } else {
            packet.bytes
        };
        self.queue_packet(Bytes::from(bytes))
            .map_err(|error| error.to_string())
    }

    fn stop_publishing(&mut self) {
        let _ = self.session.stop_publishing();
    }

    fn handle_server_input(
        &mut self,
        input: &[u8],
    ) -> Result<Vec<RtmpSessionEvent>, RtmpSessionError> {
        let results = self
            .session
            .handle_input(input)
            .map_err(|_| RtmpSessionError::Protocol("failed to parse server response"))?;
        let mut events = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    self.queue_packet(Bytes::from(packet.bytes))?;
                }
                ClientSessionResult::RaisedEvent(event) => match event {
                    ClientSessionEvent::ConnectionRequestAccepted => {
                        let packet = match self.session.request_publishing(
                            self.parts.stream_key.clone(),
                            PublishRequestType::Live,
                        ) {
                            Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
                            _ => {
                                return Err(RtmpSessionError::Protocol(
                                    "failed to build publish request",
                                ));
                            }
                        };
                        self.queue_packet(Bytes::from(packet.bytes))?;
                        events.push(RtmpSessionEvent::ConnectionRequestAccepted);
                    }
                    ClientSessionEvent::PublishRequestAccepted => {
                        events.push(RtmpSessionEvent::PublishRequestAccepted);
                    }
                    ClientSessionEvent::ConnectionRequestRejected { description } => {
                        return Err(RtmpSessionError::ConnectionRejected(description));
                    }
                    _ => {}
                },
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
        Ok(events)
    }

    fn publish_metadata(&mut self, metadata: &StreamMetadata) -> Result<(), RtmpSessionError> {
        let packet = self
            .session
            .publish_metadata(metadata)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP metadata"))?;
        self.queue_result(packet).map(|_| ())
    }

    fn publish_video_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let packet = self
            .session
            .publish_video_data(payload, timestamp, can_be_dropped)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP video packet"))?;
        self.queue_result(packet)
    }

    fn publish_audio_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let packet = self
            .session
            .publish_audio_data(payload, timestamp, can_be_dropped)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP audio packet"))?;
        self.queue_result(packet)
    }

    fn queue_result(&mut self, result: ClientSessionResult) -> Result<u64, RtmpSessionError> {
        let ClientSessionResult::OutboundResponse(packet) = result else {
            return Err(RtmpSessionError::Protocol(
                "RTMP operation returned no outbound packet",
            ));
        };
        let bytes = u64::try_from(packet.bytes.len())
            .map_err(|_| RtmpSessionError::Protocol("RTMP packet length overflow"))?;
        self.queue_packet(Bytes::from(packet.bytes))?;
        Ok(bytes)
    }

    fn queue_packet(&mut self, bytes: Bytes) -> Result<(), RtmpSessionError> {
        self.write_queue
            .try_push(bytes)
            .map_err(RtmpSessionError::Pending)
    }
}
