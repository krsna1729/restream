//! Mass-connect burst: create and start every output CONCURRENTLY instead of
//! one API round trip at a time, so a shard's Owner caller pool sees in-flight
//! handshakes, queued requests, permit release and queued admission live.
//! Enabled with `RESOURCE_SWEEP_BURST=1` (the run otherwise ramps outputs
//! serially, which can hide the pool entirely).

use std::time::Instant;

use futures_util::future::join_all;

use crate::{RampApi, RtmpOutputMode, create_output_with_rtmp_mode, start_output};

pub(super) fn burst_enabled() -> bool {
    std::env::var("RESOURCE_SWEEP_BURST").is_ok_and(|value| value == "1")
}

/// `(name, url, encoding, rtmp_mode)` for one output.
pub(super) type BurstSpec = (String, String, String, RtmpOutputMode);

/// Create every output concurrently, then start every output concurrently.
/// Returns `(output_id, start_request_instant)` per output.
pub(super) async fn create_and_start_all(
    api: &RampApi,
    pipeline_id: &str,
    specs: &[BurstSpec],
) -> Result<Vec<(String, Instant)>, String> {
    let created = join_all(specs.iter().map(|(name, url, encoding, mode)| {
        create_output_with_rtmp_mode(api, pipeline_id, name, url, encoding, *mode)
    }))
    .await;
    let mut ids = Vec::with_capacity(specs.len());
    for result in created {
        ids.push(result?);
    }
    let started = join_all(ids.iter().map(|id| async move {
        let at = Instant::now();
        start_output(api, pipeline_id, id).await.map(|()| at)
    }))
    .await;
    ids.into_iter()
        .zip(started)
        .map(|(id, at)| at.map(|at| (id, at)))
        .collect()
}
