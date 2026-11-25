//! Stable identities shared by egress commands and leaves.

use std::fmt;

/// Stable identity for a live output destination.
///
/// Carried as a thin `String` newtype so pipeline IDs, output IDs, etc.
/// cannot be accidentally mixed (mirrors `restream_domain::domain::ids` conventions).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputId(String);

impl OutputId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OutputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identity of a prepared media feed (shared ring / TsChunkRing view).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedId(String);

impl FeedId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FeedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
