//! Application-side preparation of a `moqt://` output for the MoQ fabric:
//! its feed, its `OutputSpec`, and the publish-startup snapshot the shard
//! reads (tracks, decoder configurations, broadcast name, relay trust).

use std::sync::Arc;

use crate::application::egress::PreparedOutput;
use crate::application::models::Output;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::engine::MediaEngine;
use crate::media::moq::egress::{MoqPublishStartup, parse_moq_url, publish_startup};

pub(crate) struct PreparedMoqFabricFeed {
    pub(crate) feed_id: FeedId,
    pub(crate) feed: Arc<RingFeed>,
}

pub(crate) fn prepare_moq_fabric_feed(prepared: &PreparedOutput) -> PreparedMoqFabricFeed {
    PreparedMoqFabricFeed {
        feed_id: FeedId::new(format!("moq:{}", prepared.media_stage_key)),
        feed: Arc::new(RingFeed::new(
            prepared.ring.clone(),
            Arc::new(FeedEpoch::new()),
        )),
    }
}

pub(crate) fn moq_fabric_output_spec(
    output: &Output,
    generation: u64,
    feed_id: FeedId,
) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(output.id.clone()),
        generation,
        feed: feed_id,
        protocol: ProtocolSpec::Moq {
            url: output.url.clone(),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

/// The publish startup for one output: the output ring's tracks and
/// configurations, the broadcast name (`?broadcast=`, else the pipeline id)
/// and the `ca` file. Fails for a URL the leaf cannot use or an unreadable
/// `ca` file, so the attempt records the reason instead of connecting
/// without the trust the operator asked for.
pub(crate) async fn prepare_moq_fabric_startup(
    engine: &MediaEngine,
    output: &Output,
    prepared: &PreparedOutput,
) -> Result<MoqPublishStartup, String> {
    let url = parse_moq_url(&output.url).map_err(|error| error.message().to_string())?;
    let extra_ca_pem = match url.ca_file.as_deref() {
        Some(path) => Some(
            tokio::fs::read(path)
                .await
                .map_err(|error| format!("MoQ output ca file {path} is unreadable: {error}"))?,
        ),
        None => None,
    };
    let media = publish_startup(engine, &output.pipeline_id, &prepared.ring).await;
    Ok(MoqPublishStartup {
        broadcast: url.broadcast.unwrap_or_else(|| output.pipeline_id.clone()),
        extra_ca_pem,
        ..media
    })
}

#[cfg(test)]
#[path = "egress_moq_fabric_tests.rs"]
mod tests;
