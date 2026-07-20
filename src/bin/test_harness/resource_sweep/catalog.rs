use std::sync::OnceLock;

use restream::domain::output_spec::RtmpOutputMode;
use serde::Deserialize;

use super::super::{HarnessSrtMode, harness_srt_output_url};

/// Output shape used by resource-sweep scenarios.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SweepOutputKind {
    RtmpSource,
    SrtSource,
    RtmpSourceDownmix,
    SrtSourceDownmix,
    Rtmp720p,
    Srt720p,
    Rtmp1080p,
    Srt1080p,
}

impl SweepOutputKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::RtmpSource => "rtmp-source",
            Self::SrtSource => "srt-source",
            Self::RtmpSourceDownmix => "rtmp-source-downmix",
            Self::SrtSourceDownmix => "srt-source-downmix",
            Self::Rtmp720p => "rtmp.720p.a0",
            Self::Srt720p => "srt.720p.a0",
            Self::Rtmp1080p => "rtmp.1080p.a0",
            Self::Srt1080p => "srt.1080p.a0",
        }
    }

    pub(crate) fn publish_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::RtmpSourceDownmix | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::SrtSourceDownmix | Self::Srt720p | Self::Srt1080p => {
                harness_srt_output_url(srt_port, name, HarnessSrtMode::Publish)
            }
        }
    }

    pub(crate) fn read_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::RtmpSourceDownmix | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::SrtSourceDownmix | Self::Srt720p | Self::Srt1080p => {
                harness_srt_output_url(srt_port, name, HarnessSrtMode::Read)
            }
        }
    }

    pub(crate) const fn encoding(self, multi_audio: bool) -> &'static str {
        match (self, multi_audio) {
            (Self::RtmpSource, true) => "source+atrack:0",
            (Self::SrtSource, true) => "source+atrack:0,1",
            (Self::RtmpSource | Self::SrtSource, false) => "source",
            (Self::RtmpSourceDownmix | Self::SrtSourceDownmix, _) => "source+downmix:0",
            (Self::Rtmp720p, true) => "720p+atrack:0",
            (Self::Srt720p, true) => "720p+atrack:0,1",
            (Self::Rtmp720p | Self::Srt720p, false) => "720p",
            (Self::Rtmp1080p, true) => "1080p+atrack:0",
            (Self::Srt1080p, true) => "1080p+atrack:0,1",
            (Self::Rtmp1080p | Self::Srt1080p, false) => "1080p",
        }
    }

    pub(crate) const fn rtmp_mode(self) -> RtmpOutputMode {
        match self {
            Self::RtmpSource | Self::RtmpSourceDownmix => RtmpOutputMode::Enhanced,
            Self::Rtmp720p | Self::Rtmp1080p => RtmpOutputMode::Legacy,
            Self::SrtSource | Self::SrtSourceDownmix | Self::Srt720p | Self::Srt1080p => {
                RtmpOutputMode::Legacy
            }
        }
    }
}

/// Declarative resource-sweep egress scenario row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceEgressScenario {
    pub(crate) name: String,
    pub(crate) config_index: usize,
    pub(crate) output_kinds: Vec<SweepOutputKind>,
    pub(crate) branch_order: Option<usize>,
    branch_label: Option<&'static str>,
}

impl ResourceEgressScenario {
    pub(crate) fn branch_label(&self) -> &'static str {
        self.branch_label.unwrap_or("other")
    }
}

static RESOURCE_EGRESS_SCENARIOS_FROM_DSL: OnceLock<Vec<ResourceEgressScenario>> = OnceLock::new();

pub(crate) fn resource_egress_scenarios() -> &'static [ResourceEgressScenario] {
    RESOURCE_EGRESS_SCENARIOS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("../resource_egress_scenarios.json"))
            .expect("embedded resource_egress_scenarios.json should define valid resource rows")
    })
}

pub(crate) fn resource_egress_scenario(name: &str) -> Option<&'static ResourceEgressScenario> {
    resource_egress_scenarios()
        .iter()
        .find(|scenario| scenario.name == name)
}
