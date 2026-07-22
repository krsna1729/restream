#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressProtocol {
    Rtmp,
    Srt,
    Hls,
    Sink,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputUrlScheme {
    Rtmp,
    Rtmps,
    Srt,
    Hls,
    Sink,
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
            Some("http") => Self::Http,
            Some("https") => Self::Https,
            _ => Self::Unknown,
        }
    }

    pub fn is_supported_output(self) -> bool {
        !matches!(self, Self::Unknown)
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
            Self::Unknown => "unknown",
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
