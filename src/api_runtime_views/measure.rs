//! Fixed-shape measurement snapshot (Q-037) for `GET /api/v1/measure`.
//!
//! A capacity sample needs counts and byte sums, not the operator payloads.
//! Those payloads grow with output count (about 1.5 MB each at 500 outputs,
//! see `docs/runtime-crossings.md` O2), and the harness fetched them every
//! second. This view reads the same registries and accessors as the
//! telemetry and health views but serializes a constant number of fields.

use serde::Serialize;

use super::telemetry::{delivery_input, feed_delivery};
use crate::media::engine::MediaEngine;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MeasureSnapshot {
    pub pipelines: usize,
    pub ingests: usize,
    pub egresses: usize,
    pub stages: usize,
    pub active_transcoder_buffers: usize,
    /// Sum of the active ingests' received bytes.
    pub ingest_bytes_received: u64,
    pub retained_payload_bytes: u64,
    pub source_ring_payload_bytes: u64,
    pub transcoder_ring_payload_bytes: u64,
    pub ts_muxer_ring_payload_bytes: u64,
    pub stage_input_queue_len_bytes: u64,
    pub stage_input_queue_high_water_bytes: u64,
    pub delivery: DeliveryTotals,
}

/// Restream's own delivery view summed over every feed of every pipeline.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryTotals {
    pub destinations: usize,
    pub rated: usize,
    pub delivered: usize,
    pub ratio_min: Option<f64>,
    pub jain_min: Option<f64>,
}

/// `pipelines` is the persisted pipeline count, as the health view reports it.
pub(crate) async fn measure_snapshot(engine: &MediaEngine, pipelines: usize) -> MeasureSnapshot {
    let (ingests, ingest_bytes_received) = {
        let active = engine.ingests.active.read().await;
        let bytes = active
            .values()
            .map(|ingest| {
                ingest
                    .bytes_received
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
            .sum();
        (active.len(), bytes)
    };
    let source_ring_payload_bytes = engine
        .ingests
        .pipelines
        .read()
        .await
        .values()
        .map(|ring| ring.payload_stats().payload_bytes as u64)
        .sum::<u64>();
    let (
        stages,
        active_transcoder_buffers,
        transcoder_ring_payload_bytes,
        queue_len,
        queue_high_water,
    ) = {
        // One read of `runtimes` for every stage field, so they agree within
        // a sample. `metrics` nests inside it; no writer takes them reversed.
        let runtimes = engine.stages.runtimes.read().await;
        let metrics = engine.stages.metrics.read().await;
        let stages = runtimes.len()
            + metrics
                .keys()
                .filter(|key| !runtimes.contains_key(*key))
                .count();
        drop(metrics);
        let ring_bytes = runtimes
            .values()
            .filter_map(|runtime| runtime.ring.as_ref())
            .map(|ring| ring.payload_stats().payload_bytes as u64)
            .sum::<u64>();
        let (len, high_water) = runtimes
            .values()
            .filter_map(|runtime| runtime.input_queue.as_ref())
            .map(|queue| queue.stats())
            .fold((0_u64, 0_u64), |(len, high), stats| {
                (len + stats.len as u64, high + stats.high_water_bytes as u64)
            });
        (stages, runtimes.len(), ring_bytes, len, high_water)
    };
    let ts_muxer_ring_payload_bytes = engine
        .stages
        .ts_muxers
        .read()
        .await
        .values()
        .map(|stage| stage.ring.payload_stats().payload_bytes as u64)
        .sum::<u64>();
    let (egresses, delivery) = {
        let active = engine.egresses.active.read().await;
        // Feeds are grouped per pipeline, as each pipeline's telemetry does.
        let mut by_pipeline: std::collections::BTreeMap<&str, Vec<_>> =
            std::collections::BTreeMap::new();
        for egress in active.values() {
            by_pipeline
                .entry(egress.pipeline_id.as_str())
                .or_default()
                .push(delivery_input(egress));
        }
        let mut totals = DeliveryTotals::default();
        for feed in by_pipeline.into_values().flat_map(feed_delivery) {
            totals.destinations += feed.destinations;
            totals.rated += feed.rated;
            totals.delivered += feed.delivered;
            totals.ratio_min = min_option(totals.ratio_min, feed.ratio_min);
            totals.jain_min = min_option(totals.jain_min, feed.jain);
        }
        (active.len(), totals)
    };
    MeasureSnapshot {
        pipelines,
        ingests,
        egresses,
        stages,
        active_transcoder_buffers,
        ingest_bytes_received,
        retained_payload_bytes: source_ring_payload_bytes
            + transcoder_ring_payload_bytes
            + ts_muxer_ring_payload_bytes,
        source_ring_payload_bytes,
        transcoder_ring_payload_bytes,
        ts_muxer_ring_payload_bytes,
        stage_input_queue_len_bytes: queue_len,
        stage_input_queue_high_water_bytes: queue_high_water,
        delivery,
    }
}

fn min_option(current: Option<f64>, next: Option<f64>) -> Option<f64> {
    match (current, next) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
#[path = "measure_tests.rs"]
mod tests;
