//! Persisted housekeeping switch keys shared by their owners.
//!
//! Recording and stream-check settings live in their own modules; the
//! reconciler reads their keys in one statement. Keeping the key strings
//! here lets both sides name them without importing each other.

/// The persisted recording settings row (`recording_settings`).
pub const RECORDING_SETTINGS_META_KEY: &str = "recording_settings";
/// Prefix of the per-pipeline recording switch (`recording_enabled:{id}`).
pub const RECORDING_ENABLED_META_PREFIX: &str = "recording_enabled:";
/// Prefix of the per-pipeline stream-check switch (`stream_qa_enabled:{id}`).
pub const STREAM_QA_ENABLED_META_PREFIX: &str = "stream_qa_enabled:";
