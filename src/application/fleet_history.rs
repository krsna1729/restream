//! A bounded history of the fleet's signals (Q-064): headroom, the share of
//! wanted outputs delivering, and the diagnosed outputs by layer, sampled on
//! a fixed clock whether or not anyone watches, so an incident that opened
//! before a page did can still be read against them
//! (`GET /api/v1/fleet/history?since=`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::application::diagnosis::Layer;
use crate::application::runtime_state::hub::StateHub;
use crate::application::runtime_state::{Convergence, Flow, OutputState};
use crate::capacity::CapacitySnapshot;
use crate::domain::state::DesiredOutputState;
use crate::media::engine::MediaEngine;

/// How often the sampler takes a sample.
pub const FLEET_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// How far back the history reaches: the Incidents view's longest resolved
/// window.
pub const FLEET_HISTORY_SPAN: Duration = Duration::from_secs(24 * 60 * 60);

/// Samples kept: the span at the sample interval.
pub const FLEET_HISTORY_CAP: usize =
    (FLEET_HISTORY_SPAN.as_secs() / FLEET_SAMPLE_INTERVAL.as_secs()) as usize;

/// Wanted outputs with a diagnosed fault, by the layer the fault is in.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DegradedByLayer {
    pub ingest: u32,
    pub feed: u32,
    pub shard: u32,
    pub destination: u32,
}

/// The fleet at one instant.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FleetSample {
    /// Unix epoch milliseconds.
    #[cfg_attr(test, ts(type = "number"))]
    pub at_ms: u64,
    /// 100 less the capacity snapshot's projected utilization, floored at 0.
    pub headroom_percent: f64,
    /// Wanted outputs delivering, as a percentage; `null` when none is wanted.
    pub delivering_percent: Option<f64>,
    pub degraded: DegradedByLayer,
}

/// The samples a window asked for, oldest first, and after when the kept
/// history is complete: a window that starts at or before `completeFromMs`
/// may be missing samples (the server started then, or dropped older ones).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FleetHistoryWindow {
    pub samples: Vec<FleetSample>,
    #[cfg_attr(test, ts(type = "number"))]
    pub complete_from_ms: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub interval_ms: u64,
}

/// The control room's delivering predicate (`isDelivering` in
/// `web/ts/control-room/fleet-model.ts`): wanted, converged and flowing.
pub fn is_delivering(output: &OutputState) -> bool {
    output.desired == DesiredOutputState::Running
        && output.convergence == Convergence::Converged
        && output.flow == Flow::Flowing
}

/// One sample of `outputs` (every output in the runtime state) and the
/// capacity snapshot, as the browser's `takeFleetSample` takes it.
pub fn take_fleet_sample<'a>(
    at_ms: u64,
    outputs: impl IntoIterator<Item = &'a OutputState>,
    capacity: &CapacitySnapshot,
) -> FleetSample {
    let mut wanted = 0u32;
    let mut delivering = 0u32;
    let mut degraded = DegradedByLayer::default();
    for output in outputs {
        if output.desired != DesiredOutputState::Running {
            continue;
        }
        wanted += 1;
        if is_delivering(output) {
            delivering += 1;
        }
        if let Some(diagnosis) = &output.diagnosis {
            *match diagnosis.layer {
                Layer::Ingest => &mut degraded.ingest,
                Layer::Feed => &mut degraded.feed,
                Layer::Shard => &mut degraded.shard,
                Layer::Destination => &mut degraded.destination,
            } += 1;
        }
    }
    FleetSample {
        at_ms,
        headroom_percent: (100.0 * (1.0 - f64::from(capacity.projected_utilization))).max(0.0),
        delivering_percent: (wanted > 0).then(|| 100.0 * f64::from(delivering) / f64::from(wanted)),
        degraded,
    }
}

struct HistoryState {
    samples: VecDeque<FleetSample>,
    /// After when `samples` is complete (an exclusive bound): the history's
    /// start (a restart forgets), moved to the latest evicted sample's time.
    complete_from_ms: u64,
}

/// The kept samples: at most `cap`, none older than [`FLEET_HISTORY_SPAN`]
/// before the newest; the oldest goes first.
pub struct FleetHistory {
    state: Mutex<HistoryState>,
    cap: usize,
}

impl Default for FleetHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetHistory {
    pub fn new() -> Self {
        Self::started_at(MediaEngine::now_epoch_ms(), FLEET_HISTORY_CAP)
    }

    fn started_at(start_ms: u64, cap: usize) -> Self {
        Self {
            state: Mutex::new(HistoryState {
                samples: VecDeque::new(),
                complete_from_ms: start_ms,
            }),
            cap,
        }
    }

    /// Keeps `sample`. Its time is the wall clock, which can step back (an
    /// NTP correction): it is raised to the newest kept sample's, so the
    /// samples stay in time order for [`Self::window`].
    pub fn push(&self, mut sample: FleetSample) {
        let mut state = crate::sync::lock(&self.state);
        let HistoryState {
            samples,
            complete_from_ms,
        } = &mut *state;
        if let Some(newest) = samples.back() {
            sample.at_ms = sample.at_ms.max(newest.at_ms);
        }
        let oldest_kept = sample
            .at_ms
            .saturating_sub(FLEET_HISTORY_SPAN.as_millis() as u64);
        while samples
            .front()
            .is_some_and(|front| samples.len() >= self.cap || front.at_ms < oldest_kept)
        {
            if let Some(evicted) = samples.pop_front() {
                *complete_from_ms = evicted.at_ms;
            }
        }
        samples.push_back(sample);
    }

    /// The samples taken from `since_ms` through `until_ms` (both
    /// inclusive), oldest first.
    pub fn window(&self, since_ms: u64, until_ms: u64) -> FleetHistoryWindow {
        let state = crate::sync::lock(&self.state);
        // Sorted by time: skip the others without visiting them.
        let first = state
            .samples
            .partition_point(|sample| sample.at_ms < since_ms);
        let end = state
            .samples
            .partition_point(|sample| sample.at_ms <= until_ms)
            .max(first);
        FleetHistoryWindow {
            samples: state.samples.range(first..end).cloned().collect(),
            complete_from_ms: state.complete_from_ms,
            interval_ms: FLEET_SAMPLE_INTERVAL.as_millis() as u64,
        }
    }
}

/// Samples the fleet every `interval` until `shutdown`. The projector
/// rebuilds only while a state-stream client listens, so each tick refreshes
/// the projection on demand (reusing one built in the last
/// [`crate::application::runtime_state::hub::ON_DEMAND_REUSE`]) and reads
/// it in place.
pub async fn run_fleet_sampler(
    history: Arc<FleetHistory>,
    hub: Arc<StateHub>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        hub.refresh_on_demand(&pool, &engine).await;
        let capacity = engine.capacity_snapshot().await;
        let at_ms = MediaEngine::now_epoch_ms();
        let sample = hub
            .read(|projection| take_fleet_sample(at_ms, projection.outputs.values(), &capacity))
            .await;
        history.push(sample);
    }
}

#[cfg(test)]
#[path = "fleet_history_tests.rs"]
mod tests;
