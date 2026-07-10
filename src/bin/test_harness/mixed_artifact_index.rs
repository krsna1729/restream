//! Artifact index writer for mixed harness scenarios.

use super::*;
use sha2::{Digest, Sha256};

pub(crate) const MIXED_ARTIFACT_INDEX_SCHEMA_VERSION: u32 = 2;

pub(crate) fn mixed_artifact_index_path(env: &MixedEnv) -> PathBuf {
    env.work_dir.join("artifact-index.json")
}

pub(crate) fn mixed_root_artifact_index_path(root: &Path) -> PathBuf {
    root.join("artifact-index.json")
}

pub(crate) fn write_mixed_artifact_index(env: &MixedEnv) -> Result<PathBuf, String> {
    let path = mixed_artifact_index_path(env);
    snapshot_mixed_sqlite_artifacts(env)?;
    let value = mixed_artifact_index_json(env);
    write_json_pretty_atomic(&path, &value)?;
    Ok(path)
}

pub(crate) fn write_mixed_root_artifact_index(
    root: &Path,
    mode: &str,
    scenario_path: &Path,
    root_cause_summary_path: &Path,
    assertion_log: Option<&Path>,
    cases: Vec<Value>,
) -> Result<PathBuf, String> {
    let path = mixed_root_artifact_index_path(root);
    let value = mixed_root_artifact_index_json(
        root,
        mode,
        scenario_path,
        root_cause_summary_path,
        assertion_log,
        cases,
    );
    write_json_pretty_atomic(&path, &value)?;
    Ok(path)
}

pub(crate) fn mixed_artifact_index_json(env: &MixedEnv) -> Value {
    let sqlite_snapshot_dir = mixed_sqlite_snapshot_dir(env);
    let artifacts = mixed_artifact_entries(env)
        .into_iter()
        .map(|(role, path)| mixed_artifact_entry(role, path))
        .collect::<Vec<_>>();
    let assertion_log = env.assertion_log.clone();
    json!({
        "schemaVersion": MIXED_ARTIFACT_INDEX_SCHEMA_VERSION,
        "runId": mixed_artifact_run_id(env),
        "command": std::env::args().collect::<Vec<_>>(),
        "env": mixed_artifact_env(),
        "startedAt": Utc::now().to_rfc3339(),
        "sourceRevision": mixed_artifact_source_revision(),
        "workDir": env.work_dir,
        "scenarioJson": env.work_dir.join("scenario.json"),
        "assertionsJsonl": assertion_log,
        "outputsJson": [env.outputs_json_path()],
        "stagesJson": Vec::<PathBuf>::new(),
        "logs": [env.restream_log.clone(), env.mediamtx_log.clone()],
        "media": [env.media_dir.clone()],
        "sqliteDb": env.restream_db_path,
        "sqliteSnapshotDir": sqlite_snapshot_dir,
        "artifacts": artifacts,
    })
}

fn mixed_root_artifact_index_json(
    root: &Path,
    mode: &str,
    scenario_path: &Path,
    root_cause_summary_path: &Path,
    assertion_log: Option<&Path>,
    cases: Vec<Value>,
) -> Value {
    let mut root_artifacts = vec![
        mixed_artifact_entry("scenarioJson", scenario_path.to_path_buf()),
        mixed_artifact_entry(
            "rootCauseSummaryJson",
            root_cause_summary_path.to_path_buf(),
        ),
    ];
    if let Some(assertion_log) = assertion_log {
        root_artifacts.push(mixed_artifact_entry(
            "assertionsJsonl",
            assertion_log.to_path_buf(),
        ));
    }
    json!({
        "schemaVersion": MIXED_ARTIFACT_INDEX_SCHEMA_VERSION,
        "runId": mixed_artifact_run_id_from_work_dir(root),
        "mode": mode,
        "command": std::env::args().collect::<Vec<_>>(),
        "env": mixed_artifact_env(),
        "startedAt": Utc::now().to_rfc3339(),
        "sourceRevision": mixed_artifact_source_revision(),
        "workDir": root,
        "scenarioJson": scenario_path,
        "assertionsJsonl": assertion_log,
        "rootCauseSummaryJson": root_cause_summary_path,
        "artifacts": root_artifacts,
        "cases": cases,
    })
}

fn mixed_artifact_run_id(env: &MixedEnv) -> String {
    std::env::var("RESTREAM_HARNESS_RUN_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| mixed_artifact_run_id_from_work_dir(&env.work_dir))
}

fn mixed_artifact_run_id_from_work_dir(work_dir: &Path) -> String {
    let work_name = work_dir
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("mixed");
    format!("{work_name}-{}", std::process::id())
}

fn mixed_artifact_env() -> std::collections::BTreeMap<String, String> {
    [
        "ASSERTION_LOG",
        "COLLECT_FAILURES",
        "MIXED_FAST_BREADTH_GROUPS",
        "MIXED_SIGNAL_GROUPS",
        "N_PER_GROUP",
        "ONLY_CHECKS",
        "RESTREAM_HARNESS_RUN_ID",
        "RESTREAM_MEDIA_DIR",
        "SKIP_LOAD",
        "WORK_DIR",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| (name.to_string(), value))
    })
    .collect()
}

fn mixed_artifact_source_revision() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if revision.is_empty() {
        None
    } else {
        Some(revision)
    }
}

fn mixed_artifact_entries(env: &MixedEnv) -> Vec<(&'static str, PathBuf)> {
    let sqlite_snapshot_dir = mixed_sqlite_snapshot_dir(env);
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
        ("sqliteSnapshotDir", sqlite_snapshot_dir.clone()),
        ("sqliteSnapshotDb", sqlite_snapshot_dir.join("restream.db")),
        (
            "sqliteSnapshotWal",
            sqlite_snapshot_dir.join("restream.db-wal"),
        ),
        (
            "sqliteSnapshotShm",
            sqlite_snapshot_dir.join("restream.db-shm"),
        ),
        ("mediaDir", env.media_dir.clone()),
    ];
    if let Some(assertion_log) = &env.assertion_log {
        entries.push(("assertionsJsonl", assertion_log.clone()));
    }
    entries
}

fn mixed_sqlite_snapshot_dir(env: &MixedEnv) -> PathBuf {
    env.work_dir.join("sqlite-snapshot")
}

fn snapshot_mixed_sqlite_artifacts(env: &MixedEnv) -> Result<(), String> {
    let snapshot_dir = mixed_sqlite_snapshot_dir(env);
    std::fs::create_dir_all(&snapshot_dir).map_err(|error| error.to_string())?;
    for (source, name) in [
        (env.restream_db_path.clone(), "restream.db"),
        (
            PathBuf::from(format!("{}-wal", env.restream_db_path.display())),
            "restream.db-wal",
        ),
        (
            PathBuf::from(format!("{}-shm", env.restream_db_path.display())),
            "restream.db-shm",
        ),
    ] {
        if source.exists() {
            std::fs::copy(source, snapshot_dir.join(name)).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
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
        assert!(
            index["runId"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(index["command"].as_array().is_some());
        assert!(index["env"].as_object().is_some());
        assert!(
            index["startedAt"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(index["scenarioJson"], json!(temp.join("scenario.json")));
        assert_eq!(index["outputsJson"][0], json!(env.outputs_json_path()));
        assert_eq!(
            index["assertionsJsonl"],
            json!(env.assertion_log.as_ref().expect("assertion log"))
        );
        assert_eq!(index["logs"][0], json!(env.restream_log));
        assert_eq!(index["media"][0], json!(env.media_dir));
        assert_eq!(index["sqliteDb"], json!(env.restream_db_path));
        assert_eq!(
            index["sqliteSnapshotDir"],
            json!(temp.join("sqlite-snapshot"))
        );
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
        assert!(
            artifacts
                .iter()
                .any(|entry| entry["role"] == "sqliteSnapshotDir")
        );

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn artifact_index_snapshots_sqlite_db_and_sidecars() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-sqlite-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let env = MixedEnv::from_env_with_default_work_dir("mixed.sqlite", temp.clone());
        std::fs::write(&env.restream_db_path, b"db").expect("db");
        std::fs::write(format!("{}-wal", env.restream_db_path.display()), b"wal").expect("wal");
        std::fs::write(format!("{}-shm", env.restream_db_path.display()), b"shm").expect("shm");

        let index_path = write_mixed_artifact_index(&env).expect("artifact index");
        let snapshot_dir = temp.join("sqlite-snapshot");

        assert_eq!(
            std::fs::read(snapshot_dir.join("restream.db")).expect("snapshot db"),
            b"db"
        );
        assert_eq!(
            std::fs::read(snapshot_dir.join("restream.db-wal")).expect("snapshot wal"),
            b"wal"
        );
        assert_eq!(
            std::fs::read(snapshot_dir.join("restream.db-shm")).expect("snapshot shm"),
            b"shm"
        );

        let index_body = std::fs::read_to_string(index_path).expect("artifact index body");
        let index: Value = serde_json::from_str(&index_body).expect("valid artifact index");
        let artifacts = index["artifacts"].as_array().expect("artifact list");
        for role in ["sqliteSnapshotDb", "sqliteSnapshotWal", "sqliteSnapshotShm"] {
            let entry = artifacts
                .iter()
                .find(|entry| entry["role"] == role)
                .unwrap_or_else(|| panic!("missing {role} entry"));
            assert_eq!(entry["exists"], true);
            assert!(
                entry["sha256"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            );
        }

        std::fs::remove_dir_all(temp).ok();
    }
}
