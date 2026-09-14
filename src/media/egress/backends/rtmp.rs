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
//! readiness instead of `.await`, with shard registration and application
//! startup handoff supplied by the surrounding RTMP backend.

use std::collections::VecDeque;
use std::io::{self, ErrorKind, IoSlice, Read, Write};
use std::os::unix::io::RawFd;
use std::sync::Arc;

use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use rml_rtmp::time::RtmpTimestamp;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability, WaitCondition,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::metadata::AudioMeta;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::rtmp::{
    RtmpMediaAction, RtmpMediaEncoder, RtmpSessionCore, RtmpUrlParts,
    resolve_deferred_audio_sequence_header, validate_rtmp_output_audio_packet_track,
};

use super::rtmp_connection::RtmpConnection;
use super::rtmp_handshake::{HandshakeOutcome, NonBlockingRtmpHandshake};
use rtmp_negotiation::{SessionAdvanceOutcome, SessionNegotiation};

#[path = "rtmp_negotiation.rs"]
mod rtmp_negotiation;

#[path = "rtmp_wire.rs"]
mod rtmp_wire;

use rtmp_wire::RtmpWireMessage;

const SESSION_READ_BUFFER: usize = 4096;
const MAX_VECTORED_PACKETS: usize = 16;

/// Shard-owned native TCP submission. The implementation retains the
/// pointed-to bytes until the matching CQE is delivered; `MediaPublisher`
/// keeps its `PendingWrite` in place while a send is in flight.
pub(crate) trait RtmpNativeSender {
    fn submit_send(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u64,
        bytes: &[u8],
    ) -> io::Result<()>;

    fn submit_send_vectored(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u64,
        buffers: &[&[u8]],
    ) -> io::Result<()> {
        if buffers.len() == 1 {
            self.submit_send(fd, slot, generation, buffers[0])
        } else {
            Err(io::Error::new(
                ErrorKind::Unsupported,
                "native sender has no vectored path",
            ))
        }
    }
}

pub(crate) struct RtmpNativeSend<'a> {
    pub(crate) sender: &'a mut dyn RtmpNativeSender,
    pub(crate) slot: u32,
    pub(crate) generation: u64,
    pub(crate) send_result: Option<i32>,
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

#[allow(clippy::large_enum_variant)]
enum MediaWirePacket {
    Bytes(Bytes),
    Vectored(RtmpWireMessage),
}

impl MediaWirePacket {
    fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Vectored(message) => message.remaining_len(),
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum MediaPendingWrite {
    Bytes { bytes: Bytes, offset: usize },
    Vectored(RtmpWireMessage),
}

impl MediaPendingWrite {
    fn new(packet: MediaWirePacket) -> Option<Self> {
        match packet {
            MediaWirePacket::Bytes(bytes) if bytes.is_empty() => None,
            MediaWirePacket::Bytes(bytes) => Some(Self::Bytes { bytes, offset: 0 }),
            MediaWirePacket::Vectored(message) => Some(Self::Vectored(message)),
        }
    }

    fn remaining_len(&self) -> usize {
        match self {
            Self::Bytes { bytes, offset } => bytes.len().saturating_sub(*offset),
            Self::Vectored(message) => message.remaining_len(),
        }
    }

    fn is_complete(&self) -> bool {
        self.remaining_len() == 0
    }

    fn consume(&mut self, bytes: usize) {
        match self {
            Self::Bytes {
                bytes: buffer,
                offset,
            } => {
                *offset = offset.saturating_add(bytes).min(buffer.len());
            }
            Self::Vectored(message) => message.consume(bytes),
        }
    }
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
/// `docs/archive/egress/implementation.md` Phase 4 status).
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
    current_batch: VecDeque<MediaWirePacket>,
    pending_write: Option<MediaPendingWrite>,
    native_send_pending: bool,
    /// True once a feed-derived unit's packets have been queued into
    /// `current_batch` but not yet counted as consumed — distinguishes "just
    /// finished flushing a real unit" from "nothing queued yet" so the
    /// startup batch is never miscounted as feed progress.
    unit_in_flight: bool,
    actions: Vec<RtmpMediaAction>,
    /// Units already pulled from the feed but not yet encoded. Refilled into
    /// this preallocated storage in bursts of up to `FEED_READ_BURST` units
    /// instead of allocating a new read vector for every visit.
    pending_units: Vec<Arc<MediaPacket>>,
    pending_units_index: usize,
}

/// Feed units pulled per `feed.read_from` refill once `pending_units` is
/// empty. Matches the legacy RTMP egress path's burst size.
const FEED_READ_BURST: usize = 32;

impl MediaPublisher {
    fn new(mut core: RtmpSessionCore, startup: RtmpPublishStartup) -> Result<Self, String> {
        let mut encoder = RtmpMediaEncoder::new(
            startup.enhanced_hevc_video,
            startup.raw_video_parameter_sets,
        );
        let mut current_batch = VecDeque::with_capacity(32);

        if let Some(metadata) = startup.publish_metadata.as_ref() {
            current_batch.push_back(MediaWirePacket::Bytes(
                core.publish_metadata(metadata)
                    .map_err(|error| error.to_string())?,
            ));
        }
        if let Some(video_sequence_header) = startup.startup_video_sequence_header {
            let (wire, _) = core
                .publish_video_data(video_sequence_header, RtmpTimestamp::new(0), false)
                .map_err(|error| error.to_string())?;
            current_batch.push_back(MediaWirePacket::Bytes(wire));
            encoder.set_startup_video_config(startup.startup_video_config);
        }
        let mut audio_sequence_header_sent = false;
        if let Some(audio_sequence_header) = startup.startup_audio_sequence_header {
            let (wire, _) = core
                .publish_audio_data(audio_sequence_header, RtmpTimestamp::new(0), false)
                .map_err(|error| error.to_string())?;
            current_batch.push_back(MediaWirePacket::Bytes(wire));
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
            native_send_pending: false,
            unit_in_flight: false,
            actions: Vec::with_capacity(2),
            pending_units: Vec::with_capacity(FEED_READ_BURST),
            pending_units_index: 0,
        })
    }

    /// Application bytes queued for send but not yet accepted by the
    /// transport: the remainder of any in-flight `pending_write` plus every
    /// still-queued `current_batch` packet. Used to keep
    /// `LeafCommon::pending_application_bytes` (`src/media/egress/leaf.rs`)
    /// accurate for this leaf — previously always `0` for every RTMP leaf,
    /// since nothing updated it (a hot-path audit finding: the common
    /// pending-byte limit was believed to count "the wire packet" but
    /// nothing actually wired it up at all, for any protocol). This covers
    /// the base case (queued wire bytes); rustls-internal buffering for
    /// RTMPS on top of this remains a separate, unaddressed refinement.
    fn pending_bytes(&self) -> usize {
        let pending_write_remaining = self
            .pending_write
            .as_ref()
            .map_or(0, MediaPendingWrite::remaining_len);
        let queued_batch: usize = self.current_batch.iter().map(MediaWirePacket::len).sum();
        pending_write_remaining + queued_batch
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
                self.current_batch.push_back(MediaWirePacket::Bytes(wire));
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
                } => self.media_wire_packet(9, payload, timestamp, can_be_dropped)?,
                RtmpMediaAction::Audio { payload, timestamp } => {
                    self.media_wire_packet(8, payload, timestamp, false)?
                }
            };
            self.current_batch.push_back(wire);
        }
        self.actions = actions;
        Ok(())
    }

    fn media_wire_packet(
        &mut self,
        type_id: u8,
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    ) -> Result<MediaWirePacket, String> {
        let generated = if self.core.media_stream_id().is_none() {
            Some(match type_id {
                8 => self
                    .core
                    .publish_audio_data(payload.clone(), timestamp, can_be_dropped),
                9 => self
                    .core
                    .publish_video_data(payload.clone(), timestamp, can_be_dropped),
                _ => unreachable!("only RTMP audio and video are media wire messages"),
            })
        } else {
            None
        };
        let stream_id = self.core.media_stream_id();
        let Some(stream_id) = stream_id else {
            let wire = generated
                .ok_or_else(|| "RTMP media stream id was not established".to_string())?
                .map_err(|error| error.to_string())?
                .0;
            return Ok(MediaWirePacket::Bytes(wire));
        };
        RtmpWireMessage::new(
            type_id,
            timestamp,
            stream_id,
            payload,
            self.core.chunk_size(),
        )
        .map(MediaWirePacket::Vectored)
        .map_err(str::to_string)
    }

    fn advance(
        &mut self,
        stream: &mut RtmpConnection,
        readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        self.advance_with_native(stream, readiness, feed, cursor, budget, None)
    }

    fn advance_with_native(
        &mut self,
        stream: &mut RtmpConnection,
        readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
        mut native: Option<RtmpNativeSend<'_>>,
    ) -> EngineProgress {
        let mut total_bytes = 0usize;
        let mut total_units = 0usize;

        loop {
            // Checked at the top of every pass, not just before a feed read:
            // `current_batch` (one encoded feed unit's wire packets — e.g. a
            // large keyframe split across many small RTMP chunks) used to
            // drain and write unconditionally once started, since the old
            // single budget check only sat right before `feed.read_from`.
            // One outsized unit could then fully flush in one visit,
            // ignoring `budget.max_bytes`/the visit deadline and starving
            // every other leaf on the shard for that visit's duration —
            // exactly the per-visit fairness `WorkBudget` exists to bound.
            // Cutting off here instead just defers the rest to the next
            // visit (`Self::finish` reports `Progress` if any bytes/units
            // already flowed this pass, which reschedules promptly).
            if budget.is_exhausted(total_units, total_bytes) {
                return Self::finish(
                    total_bytes,
                    total_units,
                    WaitCondition::FeedOrIo(Interest::READ_WRITE),
                );
            }

            if self.native_send_pending {
                let Some(result) = native.as_mut().and_then(|native| native.send_result.take())
                else {
                    return Self::finish(
                        total_bytes,
                        total_units,
                        WaitCondition::Io(Interest::READ_WRITE),
                    );
                };
                self.native_send_pending = false;
                if result <= 0 {
                    return EngineProgress::Failed(ProtocolFailure {
                        reason: "rtmp_media_write",
                        detail: if result == 0 {
                            "peer closed during write".to_string()
                        } else {
                            io::Error::from_raw_os_error(-result).to_string()
                        },
                        retryable: true,
                    });
                }
                let written = usize::try_from(result).unwrap_or(usize::MAX);
                total_bytes = total_bytes.saturating_add(written);
                let Some(pending) = &mut self.pending_write else {
                    return EngineProgress::Failed(ProtocolFailure {
                        reason: "rtmp_media_write",
                        detail: "native send completed without a pending buffer".to_string(),
                        retryable: false,
                    });
                };
                pending.consume(written);
                if pending.is_complete() {
                    self.pending_write = None;
                }
                if self.pending_write.is_some() {
                    continue;
                }
            }

            if let Some(pending) = &mut self.pending_write {
                if let Some(native) = native.as_mut()
                    && stream.supports_native_send()
                {
                    let result = match pending {
                        MediaPendingWrite::Bytes { bytes, offset } => native.sender.submit_send(
                            stream.raw_fd(),
                            native.slot,
                            native.generation,
                            &bytes[*offset..],
                        ),
                        MediaPendingWrite::Vectored(message) => {
                            let mut buffers: [&[u8]; MAX_VECTORED_PACKETS] =
                                [&[]; MAX_VECTORED_PACKETS];
                            let (count, _) = message
                                .fill_buffers(budget.remaining_bytes(total_bytes), &mut buffers);
                            native.sender.submit_send_vectored(
                                stream.raw_fd(),
                                native.slot,
                                native.generation,
                                &buffers[..count],
                            )
                        }
                    };
                    match result {
                        Ok(()) => {
                            self.native_send_pending = true;
                            return Self::finish(
                                total_bytes,
                                total_units,
                                WaitCondition::Io(Interest::READ_WRITE),
                            );
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            return Self::finish(
                                total_bytes,
                                total_units,
                                WaitCondition::Io(Interest::READ_WRITE),
                            );
                        }
                        Err(error) => {
                            return EngineProgress::Failed(ProtocolFailure {
                                reason: "rtmp_media_submit",
                                detail: error.to_string(),
                                retryable: true,
                            });
                        }
                    }
                }
                if !readiness.writable {
                    return Self::finish(
                        total_bytes,
                        total_units,
                        WaitCondition::Io(Interest::READ_WRITE),
                    );
                }
                let result = match pending {
                    MediaPendingWrite::Bytes { bytes, offset } => stream.write(&bytes[*offset..]),
                    MediaPendingWrite::Vectored(message) => {
                        let mut buffers: [&[u8]; MAX_VECTORED_PACKETS] =
                            [&[]; MAX_VECTORED_PACKETS];
                        let (count, _) =
                            message.fill_buffers(budget.remaining_bytes(total_bytes), &mut buffers);
                        let mut slices = [IoSlice::new(&[]); MAX_VECTORED_PACKETS];
                        for (slice, buffer) in slices.iter_mut().zip(&buffers[..count]) {
                            *slice = IoSlice::new(buffer);
                        }
                        stream.write_vectored(&slices[..count])
                    }
                };
                match result {
                    Ok(0) => {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_media_write",
                            detail: "peer closed during write".to_string(),
                            retryable: true,
                        });
                    }
                    Ok(n) => {
                        pending.consume(n);
                        total_bytes += n;
                        if !pending.is_complete() {
                            return Self::finish(
                                total_bytes,
                                total_units,
                                WaitCondition::Io(Interest::READ_WRITE),
                            );
                        }
                        self.pending_write = None;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        let hint = stream.interest_hint(Interest::WRITE);
                        return Self::finish(
                            total_bytes,
                            total_units,
                            WaitCondition::Io(Interest {
                                readable: true,
                                writable: hint.writable,
                            }),
                        );
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
                self.pending_write = MediaPendingWrite::new(next);
                continue;
            }

            if self.unit_in_flight {
                self.unit_in_flight = false;
                total_units += 1;
            }

            // Steady-state publishing is otherwise write-only: nothing here
            // ever calls `stream.read()` on the RTMP control channel, so the
            // shard poller (whose registration mirrors whatever `Interest`
            // this method returns — see `next_registration_interest` in
            // `rtmp_shard.rs`) never watches this socket for readability once
            // the initial batch is flushed. A server-sent Acknowledgement,
            // WindowAckSize, or UserControl message, or the peer closing the
            // connection, then goes undetected until the next write attempt
            // happens to fail — not a crash, but a real steady-state gap
            // (external review finding). Draining and feeding readable bytes
            // through the same `RtmpSessionCore::handle_server_input` session
            // negotiation already uses closes it: one bounded read per loop
            // pass (converges once the kernel receive buffer is drained,
            // matching `SessionNegotiation::advance`'s per-visit discipline),
            // any reply packets (e.g. an Acknowledgement) get queued for the
            // next write pass, and `Ok(0)` is treated as a real peer close
            // instead of being silently missed.
            if readiness.readable {
                let mut buffer = [0u8; SESSION_READ_BUFFER];
                match stream.read(&mut buffer) {
                    Ok(0) => {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_control_read",
                            detail: "peer closed connection".to_string(),
                            retryable: true,
                        });
                    }
                    Ok(n) => match self.core.handle_server_input(&buffer[..n]) {
                        Ok((packets, _events)) => {
                            self.current_batch
                                .extend(packets.into_iter().map(MediaWirePacket::Bytes));
                            continue;
                        }
                        Err(error) => {
                            return EngineProgress::Failed(ProtocolFailure {
                                reason: "rtmp_control_input",
                                detail: error.to_string(),
                                retryable: true,
                            });
                        }
                    },
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(error) => {
                        return EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_control_read",
                            detail: error.to_string(),
                            retryable: true,
                        });
                    }
                }
            }

            if self.pending_units_index >= self.pending_units.len() {
                self.pending_units.clear();
                self.pending_units_index = 0;
                match feed.read_from_into(
                    *cursor,
                    ReadBudget::new(FEED_READ_BURST, budget.max_bytes),
                    &mut self.pending_units,
                ) {
                    FeedRead::Units { next_cursor, .. } => *cursor = next_cursor,
                    FeedRead::Empty => {
                        return Self::finish(
                            total_bytes,
                            total_units,
                            WaitCondition::FeedOrIo(Interest::READ),
                        );
                    }
                    FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                        return EngineProgress::FeedOverrun;
                    }
                }
            }

            let Some(packet) = self.pending_units.get(self.pending_units_index).cloned() else {
                return Self::finish(
                    total_bytes,
                    total_units,
                    WaitCondition::FeedOrIo(Interest::READ),
                );
            };
            self.pending_units_index += 1;
            if let Err(detail) = self.encode_unit(&packet) {
                return EngineProgress::Failed(ProtocolFailure {
                    reason: "rtmp_media_encode",
                    detail,
                    retryable: true,
                });
            }
            self.unit_in_flight = true;
        }
    }

    fn finish(bytes: usize, units: usize, wait: WaitCondition) -> EngineProgress {
        if bytes > 0 || units > 0 {
            EngineProgress::Progress { bytes, units, wait }
        } else {
            EngineProgress::Needs(wait)
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

    /// Application bytes currently queued for send on this leaf. `0` outside
    /// `Publishing` (nothing is queued during handshake/negotiation beyond
    /// their own tiny, immediately-flushed control messages, which this
    /// intentionally does not track — see `MediaPublisher::pending_bytes`).
    pub(crate) fn pending_application_bytes(&self) -> usize {
        match &self.state {
            Some(RtmpFabricState::Publishing(publisher)) => publisher.pending_bytes(),
            _ => 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_publish_accepted(&self) -> bool {
        matches!(self.state, Some(RtmpFabricState::Publishing(_)))
    }

    /// Units already pulled from the feed into the `Publishing` state's
    /// local buffer but not yet encoded — `None` outside `Publishing`.
    #[cfg(test)]
    pub(crate) fn publisher_pending_units_len(&self) -> Option<usize> {
        match &self.state {
            Some(RtmpFabricState::Publishing(publisher)) => Some(
                publisher
                    .pending_units
                    .len()
                    .saturating_sub(publisher.pending_units_index),
            ),
            _ => None,
        }
    }

    /// Advance the publishing state with the shard's native send owner. The
    /// handshake and negotiation states intentionally keep their synchronous
    /// Rustls/RTMP writes; they run only during connection setup and may not
    /// submit raw plaintext through a not-yet-handover RTMPS socket.
    pub(crate) fn advance_native(
        &mut self,
        transport: &mut RtmpConnection,
        readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
        native: RtmpNativeSend<'_>,
    ) -> EngineProgress {
        if !matches!(self.state, Some(RtmpFabricState::Publishing(_))) {
            return <Self as ProtocolEngine>::advance(
                self, transport, readiness, feed, cursor, budget,
            );
        }
        let Some(RtmpFabricState::Publishing(mut publisher)) = self.state.take() else {
            unreachable!("publishing state was checked above")
        };
        let progress =
            publisher.advance_with_native(transport, readiness, feed, cursor, budget, Some(native));
        self.state = Some(RtmpFabricState::Publishing(publisher));
        progress
    }
}

impl ProtocolEngine for RtmpFabricEngine {
    type Feed = RingFeed;
    type Transport = RtmpConnection;

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
                        EngineProgress::Needs(WaitCondition::Io(interest))
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
                        EngineProgress::Needs(WaitCondition::Io(interest))
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
