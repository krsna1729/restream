use std::sync::Arc;

use crate::application::egress::PreparedOutput;
use crate::application::models::Output;
use crate::domain::output_spec::OutputUrlScheme;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::{FeedId, OutputId, OutputSpec, ProtocolSpec};

pub struct PreparedRtmpFabricFeed {
    pub feed_id: FeedId,
    pub feed: Arc<RingFeed>,
}

pub fn prepare_rtmp_fabric_feed(prepared: &PreparedOutput) -> PreparedRtmpFabricFeed {
    let feed_id = FeedId::new(format!("rtmp:{}", prepared.media_stage_key));

    PreparedRtmpFabricFeed {
        feed_id,
        feed: Arc::new(RingFeed::new(
            prepared.ring.clone(),
            Arc::new(FeedEpoch::new()),
        )),
    }
}

pub fn rtmp_fabric_output_spec(output: &Output, generation: u64, feed_id: FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(output.id.clone()),
        generation,
        feed: feed_id,
        protocol: ProtocolSpec::Rtmp {
            url: output.url.clone(),
            tls: matches!(
                OutputUrlScheme::from_url(&output.url),
                OutputUrlScheme::Rtmps
            ),
        },
        policy: LeafPolicy::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::application::egress::prepare_output_ring;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::state::DesiredOutputState;
    use crate::media::egress::EgressFeed;
    use crate::media::engine::MediaEngine;

    fn test_output(pipeline_id: &str, url: &str) -> Output {
        Output {
            id: format!("{pipeline_id}-out"),
            pipeline_id: pipeline_id.to_string(),
            name: "Output".to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::source(),
        }
    }

    #[tokio::test]
    async fn prepare_rtmp_fabric_feed_wraps_prepared_terminal_ring() {
        let engine = Arc::new(MediaEngine::new());
        let source = engine.get_or_create_pipeline("pipe-rtmp-fabric-feed").await;
        let output = test_output("pipe-rtmp-fabric-feed", "rtmp://example/live/key");
        let prepared = prepare_output_ring(&engine, &output).await;

        let feed = prepare_rtmp_fabric_feed(&prepared);

        assert_eq!(feed.feed_id.as_str(), "rtmp:pipe-rtmp-fabric-feed:source");
        assert!(Arc::ptr_eq(&source, &prepared.ring));
        assert_eq!(feed.feed.head_sequence(), 0);
    }

    #[test]
    fn rtmp_fabric_output_spec_maps_plain_and_tls_schemes() {
        let plain = test_output("pipe-rtmp-fabric-spec", "rtmp://localhost/live/key");
        let tls = test_output("pipe-rtmps-fabric-spec", "rtmps://localhost/live/key");

        let plain_spec = rtmp_fabric_output_spec(&plain, 11, FeedId::new("feed-rtmp"));
        let tls_spec = rtmp_fabric_output_spec(&tls, 12, FeedId::new("feed-rtmps"));

        assert_eq!(plain_spec.id.as_str(), "pipe-rtmp-fabric-spec-out");
        assert_eq!(plain_spec.generation, 11);
        assert_eq!(plain_spec.feed.as_str(), "feed-rtmp");
        match plain_spec.protocol {
            ProtocolSpec::Rtmp { url, tls } => {
                assert_eq!(url, "rtmp://localhost/live/key");
                assert!(!tls);
            }
            ProtocolSpec::Srt { .. } | ProtocolSpec::Sink | ProtocolSpec::Pipeline { .. } => {
                panic!("plain RTMP fabric spec must carry the RTMP protocol")
            }
        }

        assert_eq!(tls_spec.id.as_str(), "pipe-rtmps-fabric-spec-out");
        assert_eq!(tls_spec.generation, 12);
        assert_eq!(tls_spec.feed.as_str(), "feed-rtmps");
        match tls_spec.protocol {
            ProtocolSpec::Rtmp { url, tls } => {
                assert_eq!(url, "rtmps://localhost/live/key");
                assert!(tls);
            }
            ProtocolSpec::Srt { .. } | ProtocolSpec::Sink | ProtocolSpec::Pipeline { .. } => {
                panic!("RTMPS fabric spec must carry the RTMP protocol with TLS")
            }
        }
    }
}
