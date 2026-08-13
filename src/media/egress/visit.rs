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
        if !self.common.is_current_generation(self.generation) {
            return EngineVisitResult::StaleGeneration;
        }

        self.common.schedule.enqueued = false;
        if !self.common.cursor_primed {
            // First visit: anchor the placeholder `(0, 0)` cursor to a real
            // feed position before the engine ever reads. `LeafCommon::new`
            // cannot do this — the feed is owned by the shard and only
            // borrowed per visit — and priming here rather than at
            // construction also means the anchor is taken at the moment the
            // leaf can actually send, not when it was queued for connect.
            self.common.cursor = live_start_cursor(self.feed);
            self.common.cursor_primed = true;
            tracing::debug!(
                output_id = %self.common.output_id,
                start_epoch = self.common.cursor.epoch,
                start_sequence = self.common.cursor.next_sequence,
                head_sequence = self.feed.head_sequence(),
                "egress leaf cursor primed to feed live start"
            );
        }
        let progress = self.engine.advance(
            self.transport,
            self.readiness,
            self.feed,
            &mut self.common.cursor,
            self.budget,
        );
        let decision = apply_progress_to_common(self.common, &progress, self.feed);

        EngineVisitResult::Visited(EngineVisitOutcome { progress, decision })
    }
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
fn live_start_cursor<F: EgressFeed>(feed: &F) -> FeedCursor {
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
        EngineProgress::PeerClosed | EngineProgress::Failed(_) => VisitDecision::Close,
        EngineProgress::Yield => VisitDecision::Continue,
    }
}
