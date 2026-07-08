use crate::media::ring_buffer::DtsEnforcer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizedTs {
    pub pts_ms: i64,
    pub dts_ms: i64,
}

/// Stage-local timeline that normalizes arbitrary input PTS/DTS into a
/// single monotonic epoch shared across all streams (audio + video).
///
/// # Invariants
///
/// 1. DTS is strictly monotone per stream after normalization.
/// 2. PTS/DTS are non-negative after stage startup.
/// 3. Audio and video share one output epoch.
/// 4. File-loop timestamp resets are unwrapped into monotone output time.
/// 5. Encoded video and copied audio never mix unrelated clock origins.
#[derive(Debug, Clone)]
pub struct StageTimeline {
    /// The first observed DTS value establishes stage zero.
    base_dts_ms: Option<i64>,
    /// Accumulated offset for file-loop / reconnect resets.
    epoch_offset_ms: i64,
    /// Last raw DTS observed (for discontinuity detection).
    last_raw_dts_ms: Option<i64>,
    /// Last output DTS emitted (for discontinuity detection).
    last_out_dts_ms: Option<i64>,
    /// Per-stream DTS monotonicity enforcer.
    dts_enforcer: DtsEnforcer,
}

impl StageTimeline {
    pub fn new(num_streams: usize) -> Self {
        Self {
            base_dts_ms: None,
            epoch_offset_ms: 0,
            last_raw_dts_ms: None,
            last_out_dts_ms: None,
            dts_enforcer: DtsEnforcer::new(num_streams),
        }
    }

    /// Normalize an input (PTS, DTS) pair into the stage-local epoch.
    ///
    /// `stream_idx` identifies the stream within the per-stream DTS enforcer.
    /// Returns a `NormalizedTs` with non-negative, monotone timestamps.
    pub fn normalize(&mut self, stream_idx: usize, pts_ms: i64, dts_ms: i64) -> NormalizedTs {
        const LOOP_BACKWARD_THRESHOLD_MS: i64 = 2_000;
        const FORWARD_DISCONTINUITY_THRESHOLD_MS: i64 = 30_000;

        let base = *self.base_dts_ms.get_or_insert(dts_ms);

        if let (Some(last_raw), Some(last_out)) = (self.last_raw_dts_ms, self.last_out_dts_ms) {
            let raw_delta = dts_ms - last_raw;

            if raw_delta < -LOOP_BACKWARD_THRESHOLD_MS {
                // File loop or reconnect: keep output monotonic by adjusting
                // epoch offset so the new raw value maps to last_out + 1.
                self.epoch_offset_ms = last_out.saturating_add(1).saturating_sub(dts_ms - base);
            } else if raw_delta > FORWARD_DISCONTINUITY_THRESHOLD_MS {
                // Large forward jump: treat as discontinuity. Keep output
                // monotonic by adjusting epoch forward.
                let raw_from_base = dts_ms - base;
                if raw_from_base > last_out {
                    self.epoch_offset_ms = last_out.saturating_add(1).saturating_sub(raw_from_base);
                }
            }
        }

        let out_pts = self
            .epoch_offset_ms
            .saturating_add(pts_ms.saturating_sub(base));
        let out_dts = self
            .epoch_offset_ms
            .saturating_add(dts_ms.saturating_sub(base));

        let (out_pts, out_dts) = self.dts_enforcer.enforce(stream_idx, out_pts, out_dts);

        self.last_raw_dts_ms = Some(dts_ms);
        self.last_out_dts_ms = Some(out_dts);

        NormalizedTs {
            pts_ms: out_pts.max(0),
            dts_ms: out_dts.max(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_starts_at_zero() {
        let mut tl = StageTimeline::new(2);
        let ts = tl.normalize(0, 145_000, 144_000);
        assert_eq!(ts.pts_ms, 1_000);
        assert_eq!(ts.dts_ms, 0);
    }

    #[test]
    fn timeline_monotonic_with_increasing_input() {
        let mut tl = StageTimeline::new(2);
        let t0 = tl.normalize(0, 0, 0);
        let t1 = tl.normalize(0, 1_000, 1_000);
        let t2 = tl.normalize(0, 2_000, 2_000);
        assert!(t0.dts_ms < t1.dts_ms);
        assert!(t1.dts_ms < t2.dts_ms);
    }

    #[test]
    fn timeline_unwraps_backward_file_loop() {
        let mut tl = StageTimeline::new(2);
        tl.normalize(0, 0, 0);
        tl.normalize(0, 1_000, 1_000);
        tl.normalize(0, 2_000, 2_000);
        // File loops: PTS resets to 0
        let t_loop = tl.normalize(0, 0, 0);
        assert!(t_loop.dts_ms > 2_000, "output should remain monotonic");
        // Continue after loop
        let t_cont = tl.normalize(0, 1_000, 1_000);
        assert!(t_cont.dts_ms > t_loop.dts_ms);
    }

    #[test]
    fn timeline_unwraps_large_forward_discontinuity() {
        let mut tl = StageTimeline::new(2);
        tl.normalize(0, 0, 0);
        // Large forward jump > 30s threshold
        let t_jump = tl.normalize(0, 100_000, 100_000);
        // Should not jump to 100_000; should be clamped near previous + 1
        assert!(
            t_jump.dts_ms > 0,
            "discontinuity should not break monotonicity"
        );
    }

    #[test]
    fn timeline_keeps_audio_and_video_in_same_epoch() {
        let mut tl = StageTimeline::new(2);
        // Audio stream 0
        let a0 = tl.normalize(0, 14_370, 14_370);
        // Video stream 1
        let v0 = tl.normalize(1, 14_371, 14_371);
        // Both should be near 0/1ms, not ±14s
        assert!(
            a0.dts_ms >= 0 && a0.dts_ms < 100,
            "audio should be near epoch start"
        );
        assert!(
            v0.dts_ms >= a0.dts_ms && v0.dts_ms < 100,
            "video should be near audio"
        );
    }

    #[test]
    fn timeline_never_emits_negative_dts() {
        let mut tl = StageTimeline::new(1);
        // B-frame encoder can emit first DTS < PTS (negative relative to base)
        let ts = tl.normalize(0, 33, -16);
        assert!(
            ts.dts_ms >= 0,
            "DTS must be non-negative: got {}",
            ts.dts_ms
        );
        assert!(
            ts.pts_ms >= 0,
            "PTS must be non-negative: got {}",
            ts.pts_ms
        );
    }

    #[test]
    fn timeline_enforces_per_stream_dts_monotonicity() {
        let mut tl = StageTimeline::new(2);
        tl.normalize(0, 0, 0);
        // Out of order DTS on same stream
        let ts = tl.normalize(0, 66, -33);
        // Should be clamped to >= previous output
        assert!(ts.dts_ms >= 0);
    }

    #[test]
    fn timeline_handles_b_frames_with_earlier_dts() {
        let mut tl = StageTimeline::new(1);
        let i = tl.normalize(0, 100, 100);
        // B-frame has PTS=133, DTS=66
        let b = tl.normalize(0, 133, 66);
        // DTS should be >= 100 (previous output)
        assert!(b.dts_ms >= i.dts_ms, "B-frame DTS must not regress");
        // PTS can be > DTS
        assert!(b.pts_ms >= b.dts_ms, "B-frame PTS >= DTS");
    }
}
