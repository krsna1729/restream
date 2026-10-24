use crate::config::AppConfig;
use tracing::{info, warn};

pub(super) fn set_rlimit(limit: u64) {
    let rlimit = rustix::process::Rlimit {
        current: Some(limit),
        maximum: Some(limit),
    };
    if rustix::process::setrlimit(rustix::process::Resource::Nofile, rlimit).is_err() {
        warn!("failed to raise RLIMIT_NOFILE limit");
    } else {
        info!(limit, "raised file descriptor limit");
    }
}

pub(super) fn ensure_runtime_layout(config: &AppConfig) -> std::io::Result<()> {
    let db_path = std::path::Path::new(&config.db_path);
    if let Some(parent) = db_path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&config.media_dir)?;
    if !config.log_dir.is_empty() {
        std::fs::create_dir_all(&config.log_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_layout_creates_database_media_and_log_roots() {
        let root =
            std::env::temp_dir().join(format!("restream-runtime-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = AppConfig {
            db_path: root.join("data/restream.db").display().to_string(),
            media_dir: root.join("media").display().to_string(),
            log_dir: root.join("logs").display().to_string(),
            ..AppConfig::default()
        };

        ensure_runtime_layout(&config).unwrap();

        assert!(root.join("data").is_dir());
        assert!(root.join("media").is_dir());
        assert!(root.join("logs").is_dir());
        let _ = std::fs::remove_dir_all(root);
    }
}
