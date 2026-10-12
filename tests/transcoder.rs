//! Transcoder integration-test ownership facade.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

#[path = "transcoder/basic_external.rs"]
mod basic_external;
#[path = "transcoder/codec_edges/mod.rs"]
mod codec_edges;
#[path = "transcoder/stage_replacement.rs"]
mod stage_replacement;
#[path = "transcoder/support.rs"]
mod support;
