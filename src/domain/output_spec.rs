//! Typed output contracts shared by planning, persistence, and edge layers.
//!
//! The child modules are private implementation owners. This curated facade is
//! the stable contract surface: it preserves `domain::output_spec::*` paths
//! while allowing the internal layout to become a future contracts crate
//! without exposing file-level organization.

mod config;
mod encoding;
mod protocol;
mod video;

pub use config::{
    OutputConfig, OutputConfigError, ProtocolCapabilities, ResolvedOutputConfig,
    ResolvedOutputVideo,
};
pub use encoding::{OutputEncodingSpec, StagePresetSpec};
pub use protocol::{
    EgressProtocol, OutputProtocolConfig, OutputUrlScheme, RecirculationTarget,
    RecirculationTargetParseError, RtmpOutputMode,
};
pub use video::{OutputVideoCodec, OutputVideoConfig, VideoCodecKind, VideoSelector};

#[cfg(test)]
mod tests;
