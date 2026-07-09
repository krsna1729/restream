//! Artifact index writer for mixed harness scenarios.

use super::*;
use sha2::{Digest, Sha256};

pub(crate) const MIXED_ARTIFACT_INDEX_SCHEMA_VERSION: u32 = 1;

pub(crate) fn mixed_artifact_index_path(env: &MixedEnv) -> PathBuf {
    env.work_dir.join("artifact-index.json")
}

pub(crate) fn write_mixed_artifact_index(env: &MixedEnv) -> Result<PathBuf, String> {
    let path = mixed_artifact_index_path(env);
    let value = mixed_artifact_index_json(env);
    write_json_pretty_atomic(&path, &value)?;
    Ok(path)
}

pub(crate) fn mixed_artifact_index_json(env: &MixedEnv) -> Value {
    let artifacts = mixed_artifact_entries(env)
        .into_iter()
        .map(|(role, path)| mixed_artifact_entry(role, path))
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": MIXED_ARTIFACT_INDEX_SCHEMA_VERSION,
        "workDir": env.work_dir,
        "artifacts": artifacts,
    })
}

fn mixed_artifact_entries(env: &MixedEnv) -> Vec<(&'static str, PathBuf)> {
    let mut entries = vec![
        ("outputsJson", env.outputs_json_path()),
        ("scaleCsv", env.scale_log.clone()),
        ("timingJsonl", env.timing_log.clone()),
        ("rssSummary", env.rss_summary.clone()),
        ("summary", env.summary_log.clone()),
        ("restreamLog", env.restream_log.clone()),
        ("mediamtxLog", env.mediamtx_log.clone()),
        ("mediamtxConfig", env.mediamtx_config.clone()),
        ("restreamDb", env.restream_db_path.clone()),
        ("mediaDir", env.media_dir.clone()),
    ];
    if let Some(assertion_log) = &env.assertion_log {
        entries.push(("assertionsJsonl", assertion_log.clone()));
    }
    entries
}

fn mixed_artifact_entry(role: &'static str, path: PathBuf) -> Value {
    let metadata = std::fs::metadata(&path).ok();
    let is_file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
    json!({
        "role": role,
        "path": path,
        "exists": metadata.is_some(),
        "kind": if metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
            "directory"
        } else {
            "file"
        },
        "sizeBytes": metadata.as_ref().filter(|_| is_file).map(std::fs::Metadata::len),
        "sha256": if is_file {
            sha256_file_hex(&path).ok()
        } else {
            None
        },
    })
}

fn sha256_file_hex(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_index_records_file_checksums_and_known_roles() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-artifact-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let env = MixedEnv {
            assertion_log: Some(temp.join("assertions.jsonl")),
            ..MixedEnv::from_env_with_default_work_dir("mixed.index", temp.clone())
        };
        std::fs::write(env.outputs_json_path(), "{}").expect("outputs json");
        std::fs::write(env.assertion_log.as_ref().expect("assertion log"), "{}\n")
            .expect("assertions");

        let index = mixed_artifact_index_json(&env);

        assert_eq!(index["schemaVersion"], MIXED_ARTIFACT_INDEX_SCHEMA_VERSION);
        let artifacts = index["artifacts"].as_array().expect("artifact list");
        let outputs = artifacts
            .iter()
            .find(|entry| entry["role"] == "outputsJson")
            .expect("outputs entry");
        assert_eq!(outputs["exists"], true);
        assert_eq!(outputs["sizeBytes"], 2);
        assert_eq!(
            outputs["sha256"],
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        assert!(
            artifacts
                .iter()
                .any(|entry| entry["role"] == "assertionsJsonl")
        );

        std::fs::remove_dir_all(temp).ok();
    }
}
