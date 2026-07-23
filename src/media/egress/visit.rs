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

/// Resynchronize a leaf cursor after an overrun to the feed's latest
/// keyframe/sync-point, or to the oldest retained sequence if no sync point
/// is available (e.g. an audio-only or non-keyframe feed). The epoch is
/// re-read from the feed so a concurrent epoch bump is picked up in the same
/// step.
fn resync_cursor<F: EgressFeed>(feed: &F) -> FeedCursor {
    feed.latest_sync_point()
        .unwrap_or_else(|| FeedCursor::new(feed.epoch(), feed.oldest_sequence()))
}

fn apply_progress_to_common<F: EgressFeed>(
    common: &mut LeafCommon,
    progress: &EngineProgress,
    feed: &F,
) -> VisitDecision {
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
            common.schedule.mark_serviced();
            VisitDecision::Continue
        }
        EngineProgress::FeedOverrun => {
            common.progress.record_overrun();
            // Resynchronize in place rather than closing: the leaf keeps its
            // connection and retry budget, and resumes from a valid point
            // instead of cycling through reconnect for a transient overrun.
            common.cursor = resync_cursor(feed);
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
