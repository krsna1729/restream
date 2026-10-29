//! Application-side preparation of a `moqt://` output for the MoQ fabric:
//! its feed, its `OutputSpec`, and the publish-startup snapshot the shard
//! reads (tracks, decoder configurations, broadcast name, relay trust).

use std::ffi::OsString;
use std::path::{Component, Path};
use std::sync::Arc;

use crate::application::egress::PreparedOutput;
use crate::application::models::Output;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::engine::MediaEngine;
use crate::media::moq::egress::{MoqPublishStartup, publish_startup};
use restream_moq::url::parse_moq_url;

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

/// All path resolution and reads stay on the blocking pool. The directory
/// and its contents must be writable only by the operator, not API users.
async fn load_moq_ca_file(
    ca_file: Option<String>,
    ca_dir: Option<OsString>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(ca_file) = ca_file else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        let root = ca_dir
            .filter(|root| !root.is_empty())
            .ok_or("MoQ output ca file requires operator setting RESTREAM_MOQ_CA_DIR")?;
        let path = Path::new(&ca_file);
        // RootDir and Prefix are unreachable after the absolute check on
        // Unix; naming them keeps confinement independent of the platform.
        if path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err("MoQ output ca file must be relative to RESTREAM_MOQ_CA_DIR without `..`");
        }
        let root =
            std::fs::canonicalize(root).map_err(|_| "MoQ output CA directory is unavailable")?;
        let path = std::fs::canonicalize(root.join(path))
            .map_err(|_| "MoQ output ca file is unavailable in the configured CA directory")?;
        if !path.starts_with(&root) {
            return Err("MoQ output ca file resolves outside the configured CA directory");
        }
        std::fs::read(path)
            .map(Some)
            .map_err(|_| "MoQ output ca file is unreadable in the configured CA directory")
    })
    .await
    .map_err(|_| "MoQ output CA loading task failed".to_string())?
    .map_err(str::to_string)
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
    let ca_dir = engine.config.moq_ca_dir.clone().map(OsString::from);
    let extra_ca_pem = load_moq_ca_file(url.ca_file, ca_dir).await?;
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
