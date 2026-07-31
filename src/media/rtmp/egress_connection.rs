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
    write_queue: RtmpWriteQueue,
}

/// Pure, socket-independent RTMP client session state: owns `ClientSession`
/// (the `rml_rtmp` protocol state machine) and produces outbound packet
/// bytes without performing any I/O itself. Reused by both the legacy
/// Tokio-adapted egress path (`RtmpEgressSession` below, which wraps I/O
/// around each call) and the fabric's non-blocking engine
/// (`src/media/egress/backends/rtmp.rs`, which drives the same calls from a
/// readiness-polled shard visit instead of an `.await`ed loop).
pub(crate) struct RtmpSessionCore {
    parts: RtmpUrlParts,
    session: ClientSession,
    connect_config: ClientSessionConfig,
    initial_results: Vec<ClientSessionResult>,
}

impl RtmpSessionCore {
    pub(crate) fn new(parts: RtmpUrlParts, chunk_size: u32) -> Result<Self, String> {
        let mut config = ClientSessionConfig::new();
        let scheme = if parts.tls { "rtmps" } else { "rtmp" };
        config.tc_url = Some(format!(
            "{}://{}:{}/{}",
            scheme, parts.host, parts.port, parts.app
        ));
        config.chunk_size = chunk_size;
        let connect_config = config.clone();
        let (session, initial_results) =
            ClientSession::new(config).map_err(|error| format!("{error:?}"))?;
        Ok(Self {
            parts,
            session,
            connect_config,
            initial_results,
        })
    }
}

pub(super) enum InitialServerResultError {
    Parse,
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtmpSessionEvent {
    ConnectionRequestAccepted,
    PublishRequestAccepted,
}

#[derive(Debug)]
pub(crate) enum RtmpSessionError {
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
    pub(super) async fn connect(
        parts: RtmpUrlParts,
        buffer_size: usize,
        extra_trust_roots_pem_path: Option<&str>,
    ) -> io::Result<Self> {
        let socket =
            connect_rtmp_egress_stream(&parts, buffer_size, extra_trust_roots_pem_path).await?;
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
        let core = RtmpSessionCore::new(self.parts, chunk_size)?;
        Ok(RtmpEgressSession {
            socket: self.socket,
            remaining: self.remaining,
            core,
            write_queue: RtmpWriteQueue::new(max_pending_bytes),
        })
    }
}

impl RtmpEgressSession {
    pub(super) async fn write_initial_results(&mut self) -> io::Result<()> {
        let packets = self.core.take_initial_packets();
        self.queue_packets(packets)
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.flush_pending()
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    }

    pub(super) async fn request_connection(&mut self, enhanced: bool) -> Result<(), String> {
        let packet = self.core.request_connection(enhanced)?;
        self.queue_packet(packet)
            .map_err(|error| error.to_string())?;
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
        let (packets, _) =
            self.core
                .handle_server_input(&remaining)
                .map_err(|error| match error {
                    RtmpSessionError::Protocol(_) => InitialServerResultError::Parse,
                    RtmpSessionError::Pending(_)
                    | RtmpSessionError::Socket(_)
                    | RtmpSessionError::ConnectionRejected(_) => InitialServerResultError::Dispatch,
                })?;
        self.queue_packets(packets)
            .map_err(|_| InitialServerResultError::Dispatch)?;
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
        let (packets, events) = self.core.handle_server_input(input)?;
        self.queue_packets(packets)?;
        self.flush_pending().await?;
        Ok(events)
    }

    pub(super) async fn publish_metadata(
        &mut self,
        metadata: &StreamMetadata,
    ) -> Result<(), RtmpSessionError> {
        let packet = self.core.publish_metadata(metadata)?;
        self.queue_packet(packet)?;
        self.flush_pending().await
    }

    pub(super) async fn publish_video_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let (packet, bytes) = self
            .core
            .publish_video_data(payload, timestamp, can_be_dropped)?;
        self.queue_packet(packet)?;
        self.flush_pending().await?;
        Ok(bytes)
    }

    pub(super) async fn publish_audio_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<u64, RtmpSessionError> {
        let (packet, bytes) = self
            .core
            .publish_audio_data(payload, timestamp, can_be_dropped)?;
        self.queue_packet(packet)?;
        self.flush_pending().await?;
        Ok(bytes)
    }

    async fn flush_pending(&mut self) -> Result<(), RtmpSessionError> {
        flush_rtmp_pending_bytes(&mut self.socket, &mut self.write_queue)
            .await
            .map_err(RtmpSessionError::Socket)
    }

    fn queue_packet(&mut self, bytes: Bytes) -> Result<(), RtmpSessionError> {
        self.write_queue
            .try_push(bytes)
            .map_err(RtmpSessionError::Pending)
    }

    fn queue_packets(&mut self, packets: Vec<Bytes>) -> Result<(), RtmpSessionError> {
        for packet in packets {
            self.queue_packet(packet)?;
        }
        Ok(())
    }
}

impl RtmpSessionCore {
    pub(crate) fn take_initial_packets(&mut self) -> Vec<Bytes> {
        let initial_results = std::mem::take(&mut self.initial_results);
        initial_results
            .into_iter()
            .filter_map(|result| match result {
                ClientSessionResult::OutboundResponse(packet) => Some(Bytes::from(packet.bytes)),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn request_connection(&mut self, enhanced: bool) -> Result<Bytes, String> {
        let packet = match self.session.request_connection(self.parts.app.clone()) {
            Ok(ClientSessionResult::OutboundResponse(packet)) => packet,
            _ => return Err("failed to build connect request".to_string()),
        };
        let bytes = if enhanced {
            enhanced_rtmp_connect_packet(&self.connect_config, &self.parts.app)?
        } else {
            packet.bytes
        };
        Ok(Bytes::from(bytes))
    }

    pub(crate) fn stop_publishing(&mut self) {
        let _ = self.session.stop_publishing();
    }

    pub(crate) fn handle_server_input(
        &mut self,
        input: &[u8],
    ) -> Result<(Vec<Bytes>, Vec<RtmpSessionEvent>), RtmpSessionError> {
        let results = self
            .session
            .handle_input(input)
            .map_err(|_| RtmpSessionError::Protocol("failed to parse server response"))?;
        let mut packets = Vec::new();
        let mut events = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    packets.push(Bytes::from(packet.bytes));
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
                        packets.push(Bytes::from(packet.bytes));
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
        Ok((packets, events))
    }

    pub(crate) fn publish_metadata(
        &mut self,
        metadata: &StreamMetadata,
    ) -> Result<Bytes, RtmpSessionError> {
        let packet = self
            .session
            .publish_metadata(metadata)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP metadata"))?;
        self.packet_from_result(packet).map(|(packet, _)| packet)
    }

    pub(crate) fn publish_video_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<(Bytes, u64), RtmpSessionError> {
        let packet = self
            .session
            .publish_video_data(payload, timestamp, can_be_dropped)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP video packet"))?;
        self.packet_from_result(packet)
    }

    pub(crate) fn publish_audio_data(
        &mut self,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<(Bytes, u64), RtmpSessionError> {
        let packet = self
            .session
            .publish_audio_data(payload, timestamp, can_be_dropped)
            .map_err(|_| RtmpSessionError::Protocol("failed to build RTMP audio packet"))?;
        self.packet_from_result(packet)
    }

    fn packet_from_result(
        &mut self,
        result: ClientSessionResult,
    ) -> Result<(Bytes, u64), RtmpSessionError> {
        let ClientSessionResult::OutboundResponse(packet) = result else {
            return Err(RtmpSessionError::Protocol(
                "RTMP operation returned no outbound packet",
            ));
        };
        let bytes = u64::try_from(packet.bytes.len())
            .map_err(|_| RtmpSessionError::Protocol("RTMP packet length overflow"))?;
        Ok((Bytes::from(packet.bytes), bytes))
    }
}
