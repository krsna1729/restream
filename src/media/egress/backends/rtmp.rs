#![allow(dead_code)]

//! RTMP fabric protocol engine: the shard-scheduled, readiness-driven
//! counterpart to the RTMP fabric's [`crate::media::egress::backends::tcp`]
//! poller and [`crate::media::egress::backends::tcp_connect`] dial.
//!
//! This slice covers the full connection lifecycle through steady-state
//! media publication: the RTMP handshake (via
//! [`rtmp_handshake::NonBlockingRtmpHandshake`]), connect/publish session
//! negotiation and media encoding (both reusing
//! [`crate::media::rtmp::RtmpSessionCore`]/[`crate::media::rtmp::RtmpMediaEncoder`]
//! — the same pure, socket-independent state the existing Tokio-adapted
//! egress path uses in `src/media/rtmp/egress_connection.rs` and
//! `src/media/rtmp/egress_engine.rs`), here driven from non-blocking
//! readiness instead of `.await`. Not yet wired into a shard backend (leaf
//! registration, poller integration, application-layer startup handoff) —
//! see `docs/egress-implementation.md` Phase 5 status.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;

use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use rml_rtmp::time::RtmpTimestamp;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::metadata::AudioMeta;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::rtmp::{
    RtmpMediaAction, RtmpMediaEncoder, RtmpSessionCore, RtmpSessionError, RtmpSessionEvent,
    RtmpUrlParts, resolve_deferred_audio_sequence_header, validate_rtmp_output_audio_packet_track,
};

use super::rtmp_handshake::{HandshakeOutcome, NonBlockingRtmpHandshake};

const SESSION_READ_BUFFER: usize = 4096;

struct PendingWrite {
    bytes: Bytes,
    offset: usize,
}

impl PendingWrite {
    fn new(bytes: Bytes) -> Option<Self> {
        if bytes.is_empty() {
            None
        } else {
            Some(Self { bytes, offset: 0 })
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn is_complete(&self) -> bool {
        self.offset >= self.bytes.len()
    }
}

enum SessionAdvanceOutcome {
    Pending(Interest),
    PublishAccepted,
    Failed(String),
}

/// Drives connect/publish request negotiation over an already-handshaken
/// transport, reusing [`RtmpSessionCore`]'s pure protocol calls. Bounded to
/// at most one read or one write syscall per [`Self::advance`] call, matching
/// [`NonBlockingRtmpHandshake`]'s per-visit work discipline.
struct SessionNegotiation {
    core: RtmpSessionCore,
    outbound: VecDeque<Bytes>,
    pending_write: Option<PendingWrite>,
    unread: Vec<u8>,
    publish_accepted: bool,
}

impl SessionNegotiation {
    fn new(
        mut core: RtmpSessionCore,
        carried_over: Vec<u8>,
        enhanced: bool,
    ) -> Result<Self, String> {
        let mut outbound: VecDeque<Bytes> = core.take_initial_packets().into();
        outbound.push_back(core.request_connection(enhanced)?);
        Ok(Self {
            core,
            outbound,
            pending_write: None,
            unread: carried_over,
            publish_accepted: false,
        })
    }

    fn advance(&mut self, stream: &mut TcpStream, readiness: Readiness) -> SessionAdvanceOutcome {
        if let Some(pending) = &mut self.pending_write {
            if !readiness.writable {
                return SessionAdvanceOutcome::Pending(Interest::WRITE);
            }
            match stream.write(pending.remaining()) {
                Ok(0) => {
                    return SessionAdvanceOutcome::Failed("peer closed during write".to_string());
                }
                Ok(n) => {
                    pending.offset += n;
                    if !pending.is_complete() {
                        return SessionAdvanceOutcome::Pending(Interest::WRITE);
                    }
                    self.pending_write = None;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return SessionAdvanceOutcome::Pending(Interest::WRITE);
                }
                Err(error) => return SessionAdvanceOutcome::Failed(error.to_string()),
            }
        }

        if self.pending_write.is_none() {
            while self.pending_write.is_none() {
                match self.outbound.pop_front() {
                    Some(next) => self.pending_write = PendingWrite::new(next),
                    None => break,
                }
            }
            if self.pending_write.is_some() {
                return SessionAdvanceOutcome::Pending(Interest::WRITE);
            }
        }

        if !self.unread.is_empty() {
            let input = std::mem::take(&mut self.unread);
            return match self.core.handle_server_input(&input) {
                Ok((packets, events)) => {
                    self.outbound.extend(packets);
                    if events
                        .iter()
                        .any(|event| matches!(event, RtmpSessionEvent::PublishRequestAccepted))
                    {
                        self.publish_accepted = true;
                    }
                    let interest = if self.outbound.is_empty() {
                        Interest::READ
                    } else {
                        Interest::WRITE
                    };
                    SessionAdvanceOutcome::Pending(interest)
                }
                Err(RtmpSessionError::ConnectionRejected(description)) => {
                    SessionAdvanceOutcome::Failed(format!("connection rejected: {description}"))
                }
                Err(other) => SessionAdvanceOutcome::Failed(other.to_string()),
            };
        }

        if self.publish_accepted && self.outbound.is_empty() && self.pending_write.is_none() {
            return SessionAdvanceOutcome::PublishAccepted;
        }

        if !readiness.readable {
            return SessionAdvanceOutcome::Pending(Interest::READ);
        }

        let mut buffer = [0u8; SESSION_READ_BUFFER];
        match stream.read(&mut buffer) {
            Ok(0) => {
                SessionAdvanceOutcome::Failed("peer closed during session negotiation".to_string())
            }
            Ok(n) => {
                self.unread = buffer[..n].to_vec();
                SessionAdvanceOutcome::Pending(Interest::READ_WRITE)
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                SessionAdvanceOutcome::Pending(Interest::READ)
            }
            Err(error) => SessionAdvanceOutcome::Failed(error.to_string()),
        }
    }
}

/// Startup context needed to begin RTMP media publication once the peer
/// accepts the publish request. Deliberately mirrors
/// `crate::application::egress_rtmp_fabric::RtmpFabricStartup`'s fields
/// without the media engine depending on the application layer directly —
/// the application assembles the immutable snapshot (querying `MediaEngine`,
/// output registries, and ring state), then converts it into this
/// media-owned type before constructing the leaf; the connection-local
/// engine itself never queries anything beyond its own fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct RtmpPublishStartup {
    pub(crate) enhanced_hevc_video: bool,
    pub(crate) raw_video_parameter_sets: Vec<u8>,
    pub(crate) output_audio_track: Option<AudioMeta>,
    pub(crate) publish_metadata: Option<StreamMetadata>,
    pub(crate) startup_video_sequence_header: Option<Bytes>,
    pub(crate) startup_video_config: Option<Vec<u8>>,
    pub(crate) startup_audio_sequence_header: Option<Bytes>,
    pub(crate) deferred_audio_sequence_header: Option<Bytes>,
    pub(crate) defer_audio_until_video_ready: bool,
}

/// Drains `RingFeed` media units into non-blocking RTMP wire writes, reusing
/// [`RtmpSessionCore`]'s pure packet-building calls and [`RtmpMediaEncoder`]'s
/// pure per-packet encoding (sequence-header refresh, keyframe gating,
/// timestamp guarding) — the same logic the legacy Tokio adapter uses in
/// `src/media/rtmp/egress.rs`, factored out so both paths share it instead of
/// diverging.
///
/// Unlike the handshake and negotiation drivers (bounded to one syscall per
/// `advance()` call, since they run once and are not hot), this batches
/// multiple feed units and their wire packets into one visit, bounded by the
/// visit's [`WorkBudget`] — mirroring the SRT fabric engine's fragment
/// batching (`src/media/srt/egress_engine.rs`), which existed precisely
/// because one-wake-per-unit caused a measured CPU regression (see
/// `docs/egress-implementation.md` Phase 4 status).
struct MediaPublisher {
    core: RtmpSessionCore,
    encoder: RtmpMediaEncoder,
    output_audio_track: Option<AudioMeta>,
    audio_sequence_header_sent: bool,
    deferred_audio_sequence_header: Option<Bytes>,
    defer_audio_until_video_ready: bool,
    /// Wire packets for the batch currently being flushed: either the
    /// startup batch (metadata + sequence headers, queued once in `new` and
    /// never counted against `budget.max_units`) or one feed unit's encoded
    /// packets.
    current_batch: VecDeque<Bytes>,
    pending_write: Option<PendingWrite>,
    /// True once a feed-derived unit's packets have been queued into
    /// `current_batch` but not yet counted as consumed — distinguishes "just
    /// finished flushing a real unit" from "nothing queued yet" so the
    /// startup batch is never miscounted as feed progress.
    unit_in_flight: bool,
    actions: Vec<RtmpMediaAction>,
}

impl MediaPublisher {
    fn new(mut core: RtmpSessionCore, startup: RtmpPublishStartup) -> Result<Self, String> {
        let mut encoder = RtmpMediaEncoder::new(
            startup.enhanced_hevc_video,
            startup.raw_video_parameter_sets,
        );
        let mut current_batch = VecDeque::new();

        if let Some(metadata) = startup.publish_metadata.as_ref() {
            current_batch.push_back(
                core.publish_metadata(metadata)
                    .map_err(|error| error.to_string())?,
            );
        }
        if let Some(video_sequence_header) = startup.startup_video_sequence_header {
            let (wire, _) = core
                .publish_video_data(video_sequence_header, RtmpTimestamp::new(0), false)
                .map_err(|error| error.to_string())?;
            current_batch.push_back(wire);
            encoder.set_startup_video_config(startup.startup_video_config);
        }
        let mut audio_sequence_header_sent = false;
        if let Some(audio_sequence_header) = startup.startup_audio_sequence_header {
            let (wire, _) = core
                .publish_audio_data(audio_sequence_header, RtmpTimestamp::new(0), false)
                .map_err(|error| error.to_string())?;
            current_batch.push_back(wire);
            audio_sequence_header_sent = true;
        }

        Ok(Self {
            core,
            encoder,
            output_audio_track: startup.output_audio_track,
            audio_sequence_header_sent,
            deferred_audio_sequence_header: if audio_sequence_header_sent {
                None
            } else {
                startup.deferred_audio_sequence_header
            },
            defer_audio_until_video_ready: startup.defer_audio_until_video_ready,
            current_batch,
            pending_write: None,
            unit_in_flight: false,
            actions: Vec::with_capacity(2),
        })
    }

    /// Encode one feed unit into zero or more wire packets in
    /// `current_batch`. Mirrors the per-packet dispatch in
    /// `src/media/rtmp/egress.rs`'s media-write arm: deferred/gated audio,
    /// the audio-track validation guard, and sequence-header-before-media
    /// ordering — all pure, no engine/registry queries.
    fn encode_unit(&mut self, packet: &MediaPacket) -> Result<(), String> {
        if packet.media_type == MediaType::Audio {
            if self.defer_audio_until_video_ready && !self.encoder.video_ready() {
                return Ok(());
            }
            validate_rtmp_output_audio_packet_track(packet.track_index)?;
            if !self.audio_sequence_header_sent
                && let Some(sequence_header) = resolve_deferred_audio_sequence_header(
                    self.deferred_audio_sequence_header.as_ref(),
                    self.output_audio_track.as_ref(),
                )
            {
                let (wire, _) = self
                    .core
                    .publish_audio_data(sequence_header, RtmpTimestamp::new(0), false)
                    .map_err(|error| error.to_string())?;
                self.current_batch.push_back(wire);
                self.audio_sequence_header_sent = true;
                self.deferred_audio_sequence_header = None;
            }
            if packet.format == PayloadFormat::Raw && !self.audio_sequence_header_sent {
                // Raw AAC is not self-describing on the wire; wait until the
                // track can be announced instead of sending video-only media.
                return Ok(());
            }
        }

        let mut actions = std::mem::take(&mut self.actions);
        actions.clear();
        self.encoder.encode(packet, &mut actions);
        for action in actions.drain(..) {
            let wire = match action {
                RtmpMediaAction::Video {
                    payload,
                    timestamp,
                    can_be_dropped,
                } => {
                    self.core
                        .publish_video_data(payload, timestamp, can_be_dropped)
                        .map_err(|error| error.to_string())?
                        .0
                }
                RtmpMediaAction::Audio { payload, timestamp } => {
                    self.core
                        .publish_audio_data(payload, timestamp, false)
                        .map_err(|error| error.to_string())?
                        .0
                }
            };
            self.current_batch.push_back(wire);
        }
        self.actions = actions;
        Ok(())
    }

    fn advance(
        &mut self,
        stream: &mut TcpStream,
        readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        let mut total_bytes = 0usize;
        let mut total_units = 0usize;

        loop {
            if let Some(pending) = &mut self.pending_write {
                if !readiness.writable {
                    return Self::finish(total_bytes, total_units, Interest::WRITE);
                }
                match stream.write(pending.remaining()) {
                    Ok(0) => {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_media_write",
                            detail: "peer closed during write".to_string(),
                            retryable: true,
                        });
                    }
                    Ok(n) => {
                        pending.offset += n;
                        total_bytes += n;
                        if !pending.is_complete() {
                            return Self::finish(total_bytes, total_units, Interest::WRITE);
                        }
                        self.pending_write = None;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        return Self::finish(total_bytes, total_units, Interest::WRITE);
                    }
                    Err(error) => {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_media_write",
                            detail: error.to_string(),
                            retryable: true,
                        });
                    }
                }
            }

            if self.pending_write.is_none()
                && let Some(next) = self.current_batch.pop_front()
            {
                self.pending_write = PendingWrite::new(next);
                continue;
            }

            if self.unit_in_flight {
                self.unit_in_flight = false;
                total_units += 1;
            }

            if budget.is_exhausted(total_units, total_bytes) {
                return Self::finish(total_bytes, total_units, Interest::WRITE);
            }

            match feed.read_from(*cursor, ReadBudget::new(1, budget.max_bytes)) {
                FeedRead::Units { units, next_cursor } => {
                    *cursor = next_cursor;
                    let Some(packet) = units.into_iter().next() else {
                        return Self::finish(total_bytes, total_units, Interest::NONE);
                    };
                    if let Err(detail) = self.encode_unit(&packet) {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_media_encode",
                            detail,
                            retryable: true,
                        });
                    }
                    self.unit_in_flight = true;
                }
                FeedRead::Empty => {
                    return Self::finish(total_bytes, total_units, Interest::NONE);
                }
                FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                    return EngineProgress::FeedOverrun;
                }
            }
        }
    }

    fn finish(bytes: usize, units: usize, interest: Interest) -> EngineProgress {
        if bytes > 0 || units > 0 {
            EngineProgress::Progress {
                bytes,
                units,
                interest,
            }
        } else {
            EngineProgress::Needs(interest)
        }
    }
}

enum RtmpFabricState {
    Handshaking(Box<NonBlockingRtmpHandshake>),
    Negotiating(Box<SessionNegotiation>),
    Publishing(Box<MediaPublisher>),
}

pub(crate) struct RtmpFabricEngine {
    /// `None` only transiently, inside `advance`, while a state transition
    /// takes ownership of the previous state to build the next one — never
    /// observed outside this file.
    state: Option<RtmpFabricState>,
    /// Taken once, when the handshake completes and the session core is
    /// constructed; `None` afterward.
    parts: Option<RtmpUrlParts>,
    /// Taken once, when session negotiation completes and the media
    /// publisher is constructed; `None` afterward.
    publish_startup: Option<RtmpPublishStartup>,
    chunk_size: u32,
    enhanced: bool,
}

impl RtmpFabricEngine {
    pub(crate) fn new_client(
        parts: RtmpUrlParts,
        chunk_size: u32,
        enhanced: bool,
        publish_startup: RtmpPublishStartup,
    ) -> Result<Self, String> {
        Ok(Self {
            state: Some(RtmpFabricState::Handshaking(Box::new(
                NonBlockingRtmpHandshake::new_client()?,
            ))),
            parts: Some(parts),
            publish_startup: Some(publish_startup),
            chunk_size,
            enhanced,
        })
    }

    #[cfg(test)]
    pub(crate) fn is_handshake_done(&self) -> bool {
        !matches!(self.state, Some(RtmpFabricState::Handshaking(_)))
    }

    #[cfg(test)]
    pub(crate) fn is_publish_accepted(&self) -> bool {
        matches!(self.state, Some(RtmpFabricState::Publishing(_)))
    }
}

impl ProtocolEngine for RtmpFabricEngine {
    type Feed = RingFeed;
    type Transport = TcpStream;

    fn advance(
        &mut self,
        transport: &mut Self::Transport,
        readiness: Readiness,
        feed: &Self::Feed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        match self.state.take().expect("state is only None transiently") {
            RtmpFabricState::Handshaking(mut handshake) => {
                let outcome = handshake.advance(transport, readiness);
                match outcome {
                    HandshakeOutcome::Pending(interest) => {
                        self.state = Some(RtmpFabricState::Handshaking(handshake));
                        EngineProgress::Needs(interest)
                    }
                    HandshakeOutcome::Complete { remaining } => {
                        let parts = self
                            .parts
                            .take()
                            .expect("parts are only taken once, on this transition");
                        let core = match RtmpSessionCore::new(parts, self.chunk_size) {
                            Ok(core) => core,
                            Err(detail) => {
                                return EngineProgress::Failed(ProtocolFailure {
                                    reason: "rtmp_session_init",
                                    detail,
                                    retryable: true,
                                });
                            }
                        };
                        match SessionNegotiation::new(core, remaining, self.enhanced) {
                            Ok(negotiation) => {
                                self.state =
                                    Some(RtmpFabricState::Negotiating(Box::new(negotiation)));
                                EngineProgress::HandshakeComplete
                            }
                            Err(detail) => EngineProgress::Failed(ProtocolFailure {
                                reason: "rtmp_connect_request",
                                detail,
                                retryable: true,
                            }),
                        }
                    }
                    HandshakeOutcome::Failed(detail) => EngineProgress::Failed(ProtocolFailure {
                        reason: "rtmp_handshake",
                        detail,
                        retryable: true,
                    }),
                }
            }
            RtmpFabricState::Negotiating(mut negotiation) => {
                let outcome = negotiation.advance(transport, readiness);
                match outcome {
                    SessionAdvanceOutcome::Pending(interest) => {
                        self.state = Some(RtmpFabricState::Negotiating(negotiation));
                        EngineProgress::Needs(interest)
                    }
                    SessionAdvanceOutcome::PublishAccepted => {
                        let publish_startup = self
                            .publish_startup
                            .take()
                            .expect("publish_startup is only taken once, on this transition");
                        match MediaPublisher::new(negotiation.core, publish_startup) {
                            Ok(publisher) => {
                                self.state = Some(RtmpFabricState::Publishing(Box::new(publisher)));
                                EngineProgress::HandshakeComplete
                            }
                            Err(detail) => EngineProgress::Failed(ProtocolFailure {
                                reason: "rtmp_publish_startup",
                                detail,
                                retryable: true,
                            }),
                        }
                    }
                    SessionAdvanceOutcome::Failed(detail) => {
                        EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_session_negotiation",
                            detail,
                            retryable: true,
                        })
                    }
                }
            }
            RtmpFabricState::Publishing(mut publisher) => {
                let progress = publisher.advance(transport, readiness, feed, cursor, budget);
                self.state = Some(RtmpFabricState::Publishing(publisher));
                progress
            }
        }
    }

    fn close(&mut self, transport: &mut Self::Transport, _reason: CloseReason) {
        let _ = transport.shutdown(std::net::Shutdown::Both);
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::ReconnectOnly
    }
}

#[cfg(test)]
#[path = "rtmp_tests.rs"]
mod tests;
