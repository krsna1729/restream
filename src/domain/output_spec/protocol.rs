use crate::domain::ids::PipelineId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressProtocol {
    Rtmp,
    Srt,
    Hls,
    Sink,
    Pipeline,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputUrlScheme {
    Rtmp,
    Rtmps,
    Srt,
    Hls,
    Sink,
    Pipeline,
    Recirculate,
    Http,
    Https,
    Unknown,
}

impl OutputUrlScheme {
    pub fn from_url(url: &str) -> Self {
        match url::Url::parse(url.trim())
            .ok()
            .map(|parsed| parsed.scheme().to_ascii_lowercase())
            .as_deref()
        {
            Some("rtmp") => Self::Rtmp,
            Some("rtmps") => Self::Rtmps,
            Some("srt") => Self::Srt,
            Some("hls") => Self::Hls,
            Some("sink") => Self::Sink,
            Some("pipeline") => Self::Pipeline,
            Some("recirculate") => Self::Recirculate,
            Some("http") => Self::Http,
            Some("https") => Self::Https,
            _ => Self::Unknown,
        }
    }

    pub fn is_supported_output(self) -> bool {
        !matches!(self, Self::Pipeline | Self::Recirculate | Self::Unknown)
    }

    pub fn supports_monitoring(self) -> bool {
        matches!(self, Self::Srt | Self::Http | Self::Https)
    }

    pub fn is_rtmp_family(self) -> bool {
        matches!(self, Self::Rtmp | Self::Rtmps)
    }

    pub fn is_hls_family(self) -> bool {
        matches!(self, Self::Hls | Self::Http | Self::Https)
    }

    pub fn protocol(self) -> EgressProtocol {
        match self {
            Self::Rtmp | Self::Rtmps => EgressProtocol::Rtmp,
            Self::Srt => EgressProtocol::Srt,
            Self::Hls | Self::Http | Self::Https => EgressProtocol::Hls,
            Self::Sink => EgressProtocol::Sink,
            Self::Pipeline | Self::Recirculate => EgressProtocol::Pipeline,
            Self::Unknown => EgressProtocol::Unknown,
        }
    }
}

impl EgressProtocol {
    pub fn from_url(url: &str) -> Self {
        OutputUrlScheme::from_url(url).protocol()
    }

    pub fn is_rtmp(self) -> bool {
        matches!(self, Self::Rtmp)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
            Self::Hls => "hls",
            Self::Sink => "sink",
            Self::Pipeline => "pipeline",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecirculationTarget {
    pipeline_id: PipelineId,
    input_id: String,
}

impl RecirculationTarget {
    pub fn parse(url: &str) -> Result<Self, RecirculationTargetParseError> {
        let parsed =
            url::Url::parse(url.trim()).map_err(|_| RecirculationTargetParseError::MalformedUrl)?;
        let scheme = OutputUrlScheme::from_url(parsed.as_str());
        match scheme {
            OutputUrlScheme::Pipeline | OutputUrlScheme::Recirculate => {}
            OutputUrlScheme::Rtmp
            | OutputUrlScheme::Rtmps
            | OutputUrlScheme::Srt
            | OutputUrlScheme::Hls
            | OutputUrlScheme::Sink
            | OutputUrlScheme::Http
            | OutputUrlScheme::Https
            | OutputUrlScheme::Unknown => {
                return Err(RecirculationTargetParseError::UnsupportedScheme);
            }
        }
        let pipeline_id = parsed
            .host_str()
            .filter(|value| !value.is_empty())
            .map(PipelineId::new)
            .ok_or(RecirculationTargetParseError::MissingPipeline)?;
        let input_id = parsed
            .path_segments()
            .and_then(|mut segments| segments.find(|segment| !segment.is_empty()))
            .map(str::to_string)
            .ok_or(RecirculationTargetParseError::MissingInput)?;

        Ok(Self {
            pipeline_id,
            input_id,
        })
    }

    pub fn pipeline_id(&self) -> &str {
        self.pipeline_id.as_str()
    }

    pub fn input_id(&self) -> &str {
        &self.input_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecirculationTargetParseError {
    MalformedUrl,
    UnsupportedScheme,
    MissingPipeline,
    MissingInput,
}

impl RecirculationTargetParseError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::MalformedUrl => "recirculation URL must be a valid absolute URL",
            Self::UnsupportedScheme => {
                "recirculation URL scheme must be pipeline:// or recirculate://"
            }
            Self::MissingPipeline => "recirculation URL must include a target pipeline",
            Self::MissingInput => "recirculation URL must include a target input",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RtmpOutputMode {
    #[default]
    Legacy,
    Enhanced,
}

impl RtmpOutputMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Enhanced => "enhanced",
        }
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }

    pub fn is_enhanced(self) -> bool {
        matches!(self, Self::Enhanced)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutputProtocolConfig {
    #[default]
    Auto,
    Rtmp {
        #[serde(default)]
        mode: RtmpOutputMode,
    },
}

impl OutputProtocolConfig {
    pub fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    pub fn rtmp_mode(&self) -> Option<RtmpOutputMode> {
        match self {
            Self::Rtmp { mode } => Some(*mode),
            Self::Auto => None,
        }
    }
}
