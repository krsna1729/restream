use std::sync::atomic::Ordering;

use crate::domain::state::{EgressPhase, EgressRuntimeStatus, EgressStatus};
use crate::media::engine::{
    ActiveEgress, EGRESS_FLAP_WINDOW_MS, EGRESS_PROGRESS_STALE_MS, MediaEngine, RecentEgressOutcome,
};

impl MediaEngine {
    pub(crate) fn egress_effective_status(egress: &ActiveEgress, has_ingest: bool) -> String {
        if !has_ingest {
            return "stopped".to_string();
        }

        let phase = *egress
            .phase
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if phase == EgressPhase::Failed {
            return "failed".to_string();
        }
        if egress.status != EgressStatus::Running {
            return egress.status.to_string();
        }
        if egress.target_url.starts_with("hls://") && phase == EgressPhase::Segmenting {
            return "running".to_string();
        }

        let last_progress_ms = egress.last_progress_ms.load(Ordering::Relaxed);
        let now_ms = Self::now_epoch_ms();
        let no_progress_too_long = last_progress_ms == 0
            && egress.start_instant.elapsed().as_millis() as u64 >= EGRESS_PROGRESS_STALE_MS;
        let stale_progress = last_progress_ms > 0
            && now_ms.saturating_sub(last_progress_ms) >= EGRESS_PROGRESS_STALE_MS;
        if no_progress_too_long || stale_progress {
            return "stalled".to_string();
        }

        "running".to_string()
    }

    fn recent_egress_status(egress: &ActiveEgress, has_ingest: bool) -> EgressRuntimeStatus {
        let phase = *egress
            .phase
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if phase == EgressPhase::Failed
            || egress
                .last_error
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
        {
            return EgressRuntimeStatus::Failed;
        }
        if !has_ingest {
            return EgressRuntimeStatus::Stopped;
        }
        EgressRuntimeStatus::from(Self::egress_effective_status(egress, has_ingest))
    }

    pub(crate) fn recent_egress_flap_state(recent: Option<&RecentEgressOutcome>) -> (u32, bool) {
        let Some(recent) = recent else {
            return (0, false);
        };
        if recent.failure_count == 0 {
            return (0, false);
        }
        if Self::now_epoch_ms().saturating_sub(recent.ended_at_ms) > EGRESS_FLAP_WINDOW_MS {
            return (0, false);
        }
        (recent.failure_count, recent.failure_count >= 2)
    }

    pub(in crate::media) fn build_recent_egress_outcome(
        previous: Option<&RecentEgressOutcome>,
        egress: &ActiveEgress,
        has_ingest: bool,
        clean_stop: bool,
    ) -> RecentEgressOutcome {
        let active_phase = *egress
            .phase
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let last_error = egress
            .last_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let failure_phase = egress
            .failure_phase
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let ended_at_ms = Self::now_epoch_ms();
        let had_error =
            active_phase == EgressPhase::Failed || last_error.is_some() || failure_phase.is_some();
        let status = if had_error {
            EgressRuntimeStatus::Failed
        } else if clean_stop {
            EgressRuntimeStatus::Stopped
        } else {
            Self::recent_egress_status(egress, has_ingest)
        };
        let raw_status = if clean_stop && !had_error {
            EgressStatus::Stopped
        } else {
            egress.status
        };
        let phase = if clean_stop && !had_error {
            EgressPhase::Stopped
        } else {
            active_phase
        };
        let (first_failure_at_ms, failure_count) = if had_error {
            previous
                .filter(|previous| {
                    previous.failure_count > 0
                        && ended_at_ms.saturating_sub(previous.ended_at_ms) <= EGRESS_FLAP_WINDOW_MS
                })
                .map(|previous| {
                    (
                        if previous.first_failure_at_ms > 0 {
                            previous.first_failure_at_ms
                        } else {
                            previous.ended_at_ms
                        },
                        previous.failure_count.saturating_add(1),
                    )
                })
                .unwrap_or((ended_at_ms, 1))
        } else {
            (0, 0)
        };

        RecentEgressOutcome {
            output_id: egress.output_id.clone(),
            pipeline_id: egress.pipeline_id.clone(),
            protocol: egress.protocol.clone(),
            target_url: egress.target_url.clone(),
            target_addr: egress
                .target_addr
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            status,
            raw_status,
            phase,
            started_at: egress.started_at.clone(),
            uptime_secs: egress.start_instant.elapsed().as_secs_f64(),
            bytes_sent: egress.bytes_sent.load(Ordering::Relaxed),
            last_progress_ms: egress.last_progress_ms.load(Ordering::Relaxed),
            last_error,
            last_error_ms: egress.last_error_ms.load(Ordering::Relaxed),
            failure_phase,
            first_failure_at_ms,
            failure_count,
            quality: egress
                .quality
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
            metrics: egress.metrics.snapshot(),
            ended_at_ms,
        }
    }
}
