use crate::media::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::feed::{EgressFeed, FeedCursor};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::VisitDecision;

pub struct EngineVisit<'a, E>
where
    E: ProtocolEngine,
{
    pub generation: u64,
    pub common: &'a mut LeafCommon,
    pub engine: &'a mut E,
    pub transport: &'a mut E::Transport,
    pub readiness: Readiness,
    pub feed: &'a E::Feed,
    pub budget: WorkBudget,
}

#[derive(Debug)]
pub enum EngineVisitResult {
    StaleGeneration,
    Visited(EngineVisitOutcome),
}

#[derive(Debug)]
pub struct EngineVisitOutcome {
    pub progress: EngineProgress,
    pub decision: VisitDecision,
}

impl<E> EngineVisit<'_, E>
where
    E: ProtocolEngine,
{
    pub fn run(self) -> EngineVisitResult {
        self.run_with(|engine, transport, readiness, feed, cursor, budget| {
            E::advance(engine, transport, readiness, feed, cursor, budget)
        })
    }

    /// Variant used by native transports that need to pass one owner-thread
    /// completion/submission context into the concrete protocol engine.
    pub fn run_with(
        self,
        advance: impl FnOnce(
            &mut E,
            &mut E::Transport,
            Readiness,
            &E::Feed,
            &mut FeedCursor,
            WorkBudget,
        ) -> EngineProgress,
    ) -> EngineVisitResult {
        visit_leaf(
            self.generation,
            self.common,
            self.feed,
            self.readiness,
            self.budget,
            |cursor, readiness, budget| {
                advance(
                    self.engine,
                    self.transport,
                    readiness,
                    self.feed,
                    cursor,
                    budget,
                )
            },
        )
    }
}

/// One scheduler visit of a leaf, independent of the engine's transport shape:
/// generation check, first-visit cursor priming, the engine's own advance, and
/// the shared progress-to-decision mapping. `EngineVisit::run_with` is this
/// function plus the `ProtocolEngine` plumbing; engines whose transport lives
/// outside the leaf (SRT: the shard's Owner) call it directly.
pub(crate) fn visit_leaf<F: EgressFeed>(
    generation: u64,
    common: &mut LeafCommon,
    feed: &F,
    readiness: Readiness,
    budget: WorkBudget,
    advance: impl FnOnce(&mut FeedCursor, Readiness, WorkBudget) -> EngineProgress,
) -> EngineVisitResult {
    if !common.is_current_generation(generation) {
        return EngineVisitResult::StaleGeneration;
    }

    common.schedule.enqueued = false;
    if !common.cursor_primed {
        // First visit: anchor the placeholder `(0, 0)` cursor to a real
        // feed position before the engine ever reads. `LeafCommon::new`
        // cannot do this — the feed is owned by the shard and only
        // borrowed per visit — and priming here rather than at
        // construction also means the anchor is taken at the moment the
        // leaf can actually send, not when it was queued for connect.
        common.cursor = live_start_cursor(feed);
        common.cursor_primed = true;
        tracing::debug!(
            output_id = %common.output_id,
            start_epoch = common.cursor.epoch,
            start_sequence = common.cursor.next_sequence,
            head_sequence = feed.head_sequence(),
            "egress leaf cursor primed to feed live start"
        );
    }
    let progress = advance(&mut common.cursor, readiness, budget);
    let decision = apply_progress_to_common(common, &progress, feed);

    EngineVisitResult::Visited(EngineVisitOutcome { progress, decision })
}

/// The position a leaf should read from when it has no valid position of its
/// own: either starting fresh, or recovering from an overrun.
///
/// This is the feed's latest retained keyframe/sync point, falling back to the
/// live edge (`head_sequence`) when no sync point is retained — for example an
/// audio-only feed, or a video feed whose GOP is longer than the retention
/// window. It deliberately mirrors `RingBuffer::fast_forward`, the positioning
/// every legacy ring reader already uses (`Reader::new` on attach and the
/// overflow recovery inside `Reader::pull*`), so fabric leaves start and
/// resynchronize exactly where the pre-fabric readers did.
///
/// The fallback must not be `oldest_sequence`: that is the maximum possible
/// backward rewind, landing mid-GOP one slot away from being overwritten, so
/// the leaf immediately runs a full retention window behind live. On a live
/// transport with a latency window far smaller than its send buffer (SRT with
/// `TLPKTDROP`), that backlog is silently dropped downstream rather than
/// delivered, and nothing pulls the leaf forward again.
///
/// The epoch is re-read from the feed so a concurrent epoch bump is picked up
/// in the same step.
pub(super) fn live_start_cursor<F: EgressFeed>(feed: &F) -> FeedCursor {
    feed.latest_sync_point()
        .unwrap_or_else(|| FeedCursor::new(feed.epoch(), feed.head_sequence()))
}

fn apply_progress_to_common<F: EgressFeed>(
    common: &mut LeafCommon,
    progress: &EngineProgress,
    feed: &F,
) -> VisitDecision {
    common.schedule.wants_feed_wake = match progress {
        EngineProgress::Progress { wait, .. } | EngineProgress::Needs(wait) => wait.wants_feed(),
        EngineProgress::HandshakeComplete
        | EngineProgress::FeedOverrun
        | EngineProgress::PeerClosed
        | EngineProgress::Failed(_)
        | EngineProgress::Yield => false,
    };
    match progress {
        EngineProgress::Progress { bytes, units, .. } => {
            common.progress.record_send(*bytes, *units);
            if *bytes > 0 || *units > 0 {
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                common
                    .progress_sink
                    .record_sent(*bytes as u64, *units as u64, now_ms);
            }
            common.schedule.mark_serviced();
            VisitDecision::Continue
        }
        EngineProgress::Needs(_) => {
            common.schedule.mark_serviced();
            VisitDecision::Suspend
        }
        EngineProgress::HandshakeComplete => {
            // Handshake/negotiation states never read the feed, so the anchor
            // taken on the first visit has been aging for the whole connect +
            // handshake round trip. Re-anchor at the transition into a state
            // that can send media, so a slow handshake does not hand the
            // publisher a cursor that is already seconds behind live.
            common.cursor = live_start_cursor(feed);
            common.schedule.mark_serviced();
            VisitDecision::Continue
        }
        EngineProgress::FeedOverrun => {
            common.progress.record_overrun();
            common.progress_sink.record_overrun();
            // Resynchronize in place rather than closing: the leaf keeps its
            // connection and retry budget, and resumes from a valid point
            // instead of cycling through reconnect for a transient overrun.
            common.cursor = live_start_cursor(feed);
            tracing::warn!(
                output_id = %common.output_id,
                resync_epoch = common.cursor.epoch,
                resync_sequence = common.cursor.next_sequence,
                "egress feed overrun: leaf resynchronized to latest sync point"
            );
            VisitDecision::Continue
        }
        EngineProgress::Failed(failure) => {
            tracing::warn!(
                output_id = %common.output_id,
                reason = failure.reason,
                detail = %failure.detail,
                retryable = failure.retryable,
                "egress leaf failed"
            );
            VisitDecision::Close
        }
        EngineProgress::PeerClosed => VisitDecision::Close,
        EngineProgress::Yield => VisitDecision::Continue,
    }
}
