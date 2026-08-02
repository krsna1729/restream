use bytes::Bytes;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;

use super::egress_transport::RtmpUrlParts;
use super::enhanced::enhanced_rtmp_connect_packet;

/// Pure, socket-independent RTMP client session state: owns `ClientSession`
/// (the `rml_rtmp` protocol state machine) and produces outbound packet
/// bytes without performing any I/O itself. Driven by the fabric's
/// non-blocking engine (`src/media/egress/backends/rtmp.rs`) from a
/// readiness-polled shard visit.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtmpSessionEvent {
    ConnectionRequestAccepted,
    PublishRequestAccepted,
}

#[derive(Debug)]
pub(crate) enum RtmpSessionError {
    Protocol(&'static str),
    ConnectionRejected(String),
}

impl std::fmt::Display for RtmpSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(detail) => detail.fmt(formatter),
            Self::ConnectionRejected(description) => {
                write!(formatter, "connection request rejected: {description}")
            }
        }
    }
}

impl std::error::Error for RtmpSessionError {}

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
