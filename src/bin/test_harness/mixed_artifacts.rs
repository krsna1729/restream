//! Mixed-harness semantic artifact models.

use super::*;

pub(crate) const MIXED_OUTPUTS_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HarnessOutputCell {
    pub(crate) scenario_id: String,
    pub(crate) batch_group: String,
    pub(crate) wave: usize,
    pub(crate) pipeline_id: String,
    pub(crate) output_id: String,
    pub(crate) output_name: String,
    pub(crate) cell_id: String,
    pub(crate) duplicate_index: usize,
    pub(crate) protocol: String,
    pub(crate) encoding: String,
    pub(crate) selected_audio_track: Option<usize>,
    pub(crate) publish_url: String,
    pub(crate) read_url: Option<String>,
    pub(crate) expected_dimensions: Option<String>,
    pub(crate) expected_audio_tracks: Option<usize>,
    pub(crate) terminal_stage: Option<String>,
}

impl HarnessOutputCell {
    pub(crate) fn label(&self) -> String {
        format!(
            "{} / {} / out{}",
            self.scenario_id, self.cell_id, self.duplicate_index
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HarnessOutputRegistry {
    pub(crate) schema_version: u32,
    pub(crate) by_output_id: HashMap<String, HarnessOutputCell>,
}

impl HarnessOutputRegistry {
    pub(crate) fn new() -> Self {
        Self {
            schema_version: MIXED_OUTPUTS_SCHEMA_VERSION,
            by_output_id: HashMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, cell: HarnessOutputCell) {
        self.schema_version = MIXED_OUTPUTS_SCHEMA_VERSION;
        self.by_output_id.insert(cell.output_id.clone(), cell);
    }

    pub(crate) fn get(&self, output_id: &str) -> Option<&HarnessOutputCell> {
        self.by_output_id.get(output_id)
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut cells: Vec<_> = self.by_output_id.values().cloned().collect();
        cells.sort_by(|a, b| {
            a.scenario_id
                .cmp(&b.scenario_id)
                .then_with(|| a.cell_id.cmp(&b.cell_id))
                .then_with(|| a.duplicate_index.cmp(&b.duplicate_index))
                .then_with(|| a.output_id.cmp(&b.output_id))
        });
        json!({
            "schemaVersion": MIXED_OUTPUTS_SCHEMA_VERSION,
            "outputs": cells,
        })
    }

    pub(crate) fn write_outputs_json(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "failed to create output registry dir {}: {err}",
                    parent.display()
                )
            })?;
        }
        let body = serde_json::to_string_pretty(&self.to_json())
            .map_err(|err| format!("failed to serialize output registry: {err}"))?;
        std::fs::write(path, body)
            .map_err(|err| format!("failed to write output registry {}: {err}", path.display()))
    }
}

pub(crate) fn infer_output_protocol(url: &str) -> String {
    url.split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_registry_serializes_schema_version_and_cells() {
        let mut registry = HarnessOutputRegistry::new();
        registry.insert(HarnessOutputCell {
            scenario_id: "mixed.live.rtmp.h264.a1.bf0".to_string(),
            batch_group: "rtmp.source".to_string(),
            wave: 0,
            pipeline_id: "pipe".to_string(),
            output_id: "out-1".to_string(),
            output_name: "rtmp.source-1".to_string(),
            cell_id: "rtmp.source".to_string(),
            duplicate_index: 1,
            protocol: "rtmp".to_string(),
            encoding: "source".to_string(),
            selected_audio_track: None,
            publish_url: "rtmp://127.0.0.1/live/out".to_string(),
            read_url: None,
            expected_dimensions: Some("1920x1080".to_string()),
            expected_audio_tracks: Some(1),
            terminal_stage: None,
        });

        let json = registry.to_json();
        assert_eq!(json["schemaVersion"], MIXED_OUTPUTS_SCHEMA_VERSION);
        assert_eq!(json["outputs"][0]["outputId"], "out-1");
        assert_eq!(
            registry.get("out-1").expect("cell registered").label(),
            "mixed.live.rtmp.h264.a1.bf0 / rtmp.source / out1"
        );
    }

    #[test]
    fn infer_output_protocol_reads_url_scheme() {
        assert_eq!(infer_output_protocol("srt://127.0.0.1:9000"), "srt");
        assert_eq!(infer_output_protocol("rtmp://localhost/live/out"), "rtmp");
        assert_eq!(infer_output_protocol("not-a-url"), "unknown");
    }
}
