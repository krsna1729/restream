//! Recording domain configuration shared by persistence and runtime writers.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingSettings {
    pub retain_source_ts: bool,
}
