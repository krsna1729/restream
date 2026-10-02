//! One service-demand model for RTMP and SRT. CPU clocks exclude parking;
//! delivery and fairness validate the work before learning its CPU cost.
use std::time::{Duration, Instant};

const TARGET: f64 = 0.50;
// Merged load must stay 10 points under TARGET after a shrink, so one more
// output cannot immediately call for the shard back.
const SHRINK_TARGET: f64 = 0.40;
const SHRINK_DWELL: Duration = Duration::from_secs(300);
/// Consecutive unhealthy windows that count as trouble, not a blip.
const BLIP_WINDOWS: u32 = 3;
const REFERENCE_FEED_BPS: f64 = 4.8e6;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ServiceSample {
    pub shards: u32,
    pub cpu_cores: f64,
    pub peak_utilization: f64,
    pub rated: u32,
    pub under_floor: u32,
    pub offered_bps: f64,
    pub delivered_bps: f64,
    pub fairness: Option<f64>,
}

impl ServiceSample {
    fn healthy(self) -> bool {
        self.rated > 0
            && self.under_floor == 0
            && self.fairness.is_none_or(|value| value >= 0.97)
            && self.delivered_bps > 0.0
            && self.offered_bps > 0.0
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ShardSizer {
    // CPU seconds per completed bit (D); feed_bps is arrival rate per output.
    service_demand: Option<f64>,
    feed_bps: Option<f64>,
    low_windows: u32,
    /// Consecutive windows that were not healthy.
    unhealthy_run: u32,
    last_resize: Option<Instant>,
}

impl ShardSizer {
    pub(crate) fn forecast_feed(&mut self, bps: f64) {
        if bps.is_finite() && bps > 0.0 {
            self.feed_bps = Some(bps);
        }
    }

    fn demand_per_output(&self, prior: u32) -> f64 {
        let d = self
            .service_demand
            .unwrap_or(TARGET / (f64::from(prior) * REFERENCE_FEED_BPS));
        self.feed_bps.unwrap_or(REFERENCE_FEED_BPS) * d
    }

    /// Measure D = CPU time / completed bytes, then lambda*D using offered
    /// bytes. Do not learn from stalled peers, missing rates or partial startup.
    /// Existing delivery windows are five seconds; call once per fresh window.
    pub(crate) fn observe(&mut self, sample: ServiceSample, outputs: usize) {
        let healthy = sample.healthy() && sample.rated as usize >= outputs;
        if self.feed_bps.is_none() && sample.rated > 0 {
            self.forecast_feed(sample.offered_bps / f64::from(sample.rated));
        }
        if healthy && sample.cpu_cores >= 0.05 {
            let demand = sample.cpu_cores / sample.delivered_bps;
            if demand.is_finite() && demand > 0.0 {
                self.service_demand = Some(self.service_demand.map_or(demand, |old| {
                    let weight = if demand > old { 0.5 } else { 0.1 };
                    old + weight * (demand - old)
                }));
            }
        }
        // Poor delivery alone might be a remote fault. Local CPU pressure must
        // corroborate it before adding shards; Jain=1 can still mean all starve.
        let widespread =
            sample.under_floor >= 2 && sample.under_floor.saturating_mul(4) >= sample.rated;
        let local_pressure = sample.rated as usize >= outputs
            && sample.rated > 0
            && (sample.peak_utilization >= TARGET
                || (widespread && sample.peak_utilization >= 0.40));
        let projected_on_fewer =
            sample.cpu_cores / f64::from(sample.shards.saturating_sub(1).max(1));
        self.unhealthy_run = if healthy { 0 } else { self.unhealthy_run + 1 };
        if projected_on_fewer >= SHRINK_TARGET || self.unhealthy_run >= BLIP_WINDOWS {
            self.low_windows = 0;
        } else if healthy {
            self.low_windows = self.low_windows.saturating_add(1);
        }
        // One or two unhealthy 5 s delivery windows (measured every ~2 min at
        // low load on the reference host) hold the streak; they neither count
        // toward a shrink nor cancel it.
        if local_pressure && sample.rated > 0 {
            // A failed delivery window cannot provide a trustworthy D. Keep a
            // conservative lower bound from actual CPU and offered output count.
            if sample.offered_bps > 0.0 {
                let lower = sample.cpu_cores / sample.offered_bps;
                self.service_demand = Some(self.service_demand.unwrap_or(0.0).max(lower));
            }
        }
    }

    /// Plan the pending Add using the same law as runtime adjustment, before
    /// creating its connection. `cold_outputs_per_shard` is only a startup prior.
    pub(crate) fn recommend(
        &self,
        outputs: usize,
        cold_outputs_per_shard: u32,
        maximum: u32,
    ) -> u32 {
        let demand = self.demand_per_output(cold_outputs_per_shard);
        ((outputs as f64 * demand / TARGET).ceil() as u32).clamp(1, maximum.max(1))
    }

    /// Periodic decision. Existing outputs never move, so pressure on them
    /// cannot be relieved by a new shard: growth happens only for new outputs
    /// (`recommend`). This can only ask to stop placing on the tail shard,
    /// after sustained low load, a long dwell, and projected headroom with one
    /// shard fewer.
    pub(crate) fn shrink_target(
        &self,
        now: Instant,
        outputs: usize,
        placement: u32,
        cold_outputs_per_shard: u32,
    ) -> u32 {
        let age = self
            .last_resize
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        let projected = self.demand_per_output(cold_outputs_per_shard) * outputs as f64
            / f64::from(placement.saturating_sub(1).max(1));
        if placement > 1
            && self.low_windows >= 60
            && age >= SHRINK_DWELL
            && projected < SHRINK_TARGET
        {
            placement - 1
        } else {
            placement
        }
    }

    /// Drop the shrink streak after a missing or unreliable measurement.
    pub(crate) fn forget_headroom(&mut self) {
        self.low_windows = 0;
    }

    /// Commit only a real topology change; reset the healthy-low history.
    pub(crate) fn resized(&mut self, now: Instant) {
        self.last_resize = Some(now);
        self.low_windows = 0;
        self.unhealthy_run = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn sample(cpu: f64, peak: f64, outputs: u32) -> ServiceSample {
        ServiceSample {
            shards: 1,
            cpu_cores: cpu,
            peak_utilization: peak,
            rated: outputs,
            offered_bps: f64::from(outputs) * REFERENCE_FEED_BPS,
            delivered_bps: f64::from(outputs) * REFERENCE_FEED_BPS,
            fairness: Some(1.0),
            ..Default::default()
        }
    }
    #[test]
    fn recommendations_use_service_demand_and_include_pending_output() {
        let mut sizer = ShardSizer::default();
        assert_eq!(sizer.recommend(64, 64, 8), 1);
        assert_eq!(sizer.recommend(65, 64, 8), 2);
        sizer.observe(sample(0.5, 0.5, 16), 16);
        assert_eq!(sizer.recommend(16, 64, 8), 1);
        assert_eq!(sizer.recommend(17, 64, 8), 2);
        assert_eq!(sizer.recommend(80, 64, 3), 3);
        sizer.forecast_feed(REFERENCE_FEED_BPS * 2.0);
        assert_eq!(
            sizer.recommend(16, 64, 8),
            2,
            "doubling feed rate doubles planned CPU demand"
        );
    }

    #[test]
    fn an_old_delivery_window_cannot_overwrite_a_new_arrival_rate() {
        let mut sizer = ShardSizer::default();
        let previous = sample(0.5, 0.5, 16);
        sizer.observe(previous, 16);
        sizer.forecast_feed(REFERENCE_FEED_BPS * 2.0);
        sizer.observe(previous, 16);
        assert_eq!(sizer.recommend(16, 64, 8), 2);
    }
    #[test]
    fn remote_stalls_missing_rates_and_partial_startup_do_not_teach_capacity() {
        let mut sizer = ShardSizer::default();
        for sample in [
            ServiceSample {
                under_floor: 1,
                ..sample(0.2, 0.2, 4)
            },
            ServiceSample {
                delivered_bps: 0.0,
                ..sample(0.2, 0.2, 4)
            },
            sample(0.2, 0.2, 1),
        ] {
            sizer.observe(sample, 4);
            assert!(sizer.service_demand.is_none());
        }
        assert_eq!(sizer.recommend(64, 64, 8), 1);
    }
    #[test]
    fn local_pressure_teaches_the_next_placement_but_never_moves_live_outputs() {
        let mut sizer = ShardSizer::default();
        assert_eq!(sizer.recommend(16, 64, 8), 1);
        sizer.observe(
            ServiceSample {
                under_floor: 16,
                ..sample(0.9, 0.9, 16)
            },
            16,
        );
        assert!(
            sizer.recommend(17, 64, 8) > 1,
            "the next Add is placed for the measured cost"
        );
        assert_eq!(
            sizer.shrink_target(Instant::now(), 16, 1, 64),
            1,
            "pressure never produces a topology change by itself"
        );
    }
    #[test]
    fn shrink_requires_projected_headroom_long_dwell_and_healthy_windows() {
        let start = Instant::now();
        let mut sizer = ShardSizer::default();
        sizer.resized(start);
        let low_load = ServiceSample {
            shards: 3,
            ..sample(0.10, 0.10, 16)
        };
        for _ in 0..59 {
            sizer.observe(low_load, 16);
        }
        assert_eq!(sizer.shrink_target(start + SHRINK_DWELL, 16, 3, 64), 3);
        sizer.observe(low_load, 16);
        assert_eq!(sizer.shrink_target(start, 16, 3, 64), 3);
        assert_eq!(sizer.shrink_target(start + SHRINK_DWELL, 16, 3, 64), 2);
        sizer.resized(start + SHRINK_DWELL);
        assert_eq!(sizer.shrink_target(start + SHRINK_DWELL * 2, 16, 2, 64), 2);
    }
    #[test]
    fn healthy_busy_windows_cannot_authorize_shrink_on_one_brief_demand_drop() {
        let start = Instant::now();
        let mut sizer = ShardSizer::default();
        sizer.resized(start);
        for _ in 0..100 {
            sizer.observe(
                ServiceSample {
                    shards: 2,
                    ..sample(0.6, 0.3, 16)
                },
                16,
            );
        }
        sizer.forecast_feed(REFERENCE_FEED_BPS / 4.0);
        assert_eq!(sizer.recommend(16, 64, 8), 1);
        assert_eq!(
            sizer.shrink_target(start + Duration::from_secs(1000), 16, 2, 64),
            2,
            "healthy high-load history is not sustained low-load history"
        );
    }

    #[test]
    fn isolated_delivery_blips_do_not_restart_the_shrink_streak_but_sustained_trouble_does() {
        let start = Instant::now();
        let low = ServiceSample {
            shards: 3,
            ..sample(0.10, 0.10, 16)
        };
        let blip = ServiceSample {
            under_floor: 16,
            ..low
        };
        let mut sizer = ShardSizer::default();
        sizer.resized(start);
        for i in 0..90 {
            // one bad window in every ten, like the noise measured on the VPS
            sizer.observe(if i % 10 == 9 { blip } else { low }, 16);
        }
        assert_eq!(sizer.shrink_target(start + SHRINK_DWELL, 16, 3, 64), 2);

        let mut sizer = ShardSizer::default();
        sizer.resized(start);
        for _ in 0..70 {
            sizer.observe(low, 16);
        }
        for _ in 0..3 {
            sizer.observe(blip, 16);
        }
        assert_eq!(
            sizer.shrink_target(start + SHRINK_DWELL, 16, 3, 64),
            3,
            "three bad windows in a row are trouble"
        );
    }

    #[test]
    fn unfair_remote_destination_does_not_add_threads() {
        let start = Instant::now();
        let mut sizer = ShardSizer::default();
        sizer.resized(start);
        for _ in 0..200 {
            sizer.observe(
                ServiceSample {
                    under_floor: 1,
                    fairness: Some(0.6),
                    ..sample(0.1, 0.1, 4)
                },
                4,
            );
        }
        assert_eq!(
            sizer.shrink_target(start + SHRINK_DWELL * 2, 4, 2, 64),
            2,
            "a slow remote destination is not spare local capacity"
        );
    }
    proptest! {
        #[test]
        fn recommendations_are_bounded_and_monotone(a in 0usize..10000, b in 0usize..10000,
            prior in 1u32..256, max in 1u32..9) {
            let sizer = ShardSizer::default();
            let lo=sizer.recommend(a.min(b),prior,max); let hi=sizer.recommend(a.max(b),prior,max);
            prop_assert!(lo >= 1 && hi <= max && lo <= hi);
        }
        #[test]
        fn noisy_load_never_grows_and_shrinks_only_after_each_dwell(
            noise in proptest::collection::vec(0.0f64..0.8, 100..600)
        ) {
            let start=Instant::now(); let mut sizer=ShardSizer::default(); sizer.resized(start);
            let mut placement=4u32; let mut last=start;
            for (i,cpu) in noise.iter().enumerate() {
                sizer.observe(ServiceSample { shards: placement, ..sample(*cpu, *cpu / f64::from(placement), 16) }, 16);
                let now=start+Duration::from_secs(i as u64*5);
                let next=sizer.shrink_target(now,16,placement,64);
                prop_assert!(next <= placement, "periodic observation cannot add shards");
                if next < placement {
                    prop_assert!(now.duration_since(last) >= SHRINK_DWELL, "shrink waited out the dwell");
                    placement=next; last=now; sizer.resized(now);
                }
            }
        }
    }
}
