use std::collections::HashSet;
use std::path::Path;

use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::api::auth::hash_password;
use crate::api::state::{BOOTSTRAP_PASSWORD_PROMPT_META_KEY, SESSION_MAX_AGE_SECONDS, to_hex};
use crate::application::services::AuthService;
use crate::infrastructure::service_wiring::SqliteServiceFactory;

const BOOTSTRAP_PROMPT_PENDING: &str = "pending";
const BOOTSTRAP_PROMPT_DISMISSED: &str = "dismissed";

pub(super) async fn initialize_auth_with_bootstrap_file(
    auth_service: &AuthService,
    sessions: &RwLock<HashSet<String>>,
    bootstrap_password_file: Option<&Path>,
    initial_admin_password: Option<&str>,
) {
    if matches!(auth_service.get_password_hash().await, Ok(None)) {
        let (password, generated) =
            select_initial_admin_password(initial_admin_password.map(str::to_string));
        let admin_hash = hash_password(&password);
        if let Err(error) = auth_service.ensure_password_hash(&admin_hash).await {
            panic!("failed to initialize dashboard password: {error}");
        }
        if generated {
            if let Some(path) = bootstrap_password_file {
                write_bootstrap_password_file(path, &password)
                    .unwrap_or_else(|error| panic!("failed to write bootstrap password: {error}"));
                info!(
                    path = %path.display(),
                    "generated initial dashboard password; read it from this local file"
                );
            } else {
                info!(
                    password = %password,
                    "generated initial dashboard password"
                );
            }
            let _ = auth_service
                .set_meta(BOOTSTRAP_PASSWORD_PROMPT_META_KEY, BOOTSTRAP_PROMPT_PENDING)
                .await;
        } else {
            let _ = auth_service
                .set_meta(
                    BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
                    BOOTSTRAP_PROMPT_DISMISSED,
                )
                .await;
        }
    }

    let _ = auth_service
        .prune_expired_sessions(SESSION_MAX_AGE_SECONDS * 1000)
        .await;
    match auth_service.list_sessions().await {
        Ok(tokens) => {
            let mut sessions = sessions.write().await;
            sessions.extend(tokens);
        }
        Err(error) => {
            warn!(err = %error, "Failed to load active sessions from SQLite");
        }
    }
}

pub async fn initialize_auth_for_test(
    pool: &SqlitePool,
    sessions: &RwLock<HashSet<String>>,
    password: &str,
) {
    let auth_service = SqliteServiceFactory::new(pool).auth_service();
    auth_service
        .set_password_hash(&hash_password(password))
        .await
        .expect("test auth password should persist");
    let _ = auth_service
        .set_meta(
            BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
            BOOTSTRAP_PROMPT_DISMISSED,
        )
        .await;
    initialize_auth_with_bootstrap_file(&auth_service, sessions, None, None).await;
}

fn select_initial_admin_password(configured_password: Option<String>) -> (String, bool) {
    match configured_password {
        Some(value) if !value.is_empty() => (value, false),
        Some(_) | None => (generate_bootstrap_password(), true),
    }
}

fn generate_bootstrap_password() -> String {
    use rand::RngExt;

    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    to_hex(&bytes)
}

fn write_bootstrap_password_file(path: &Path, password: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        writeln!(file, "{password}")?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, format!("{password}\n"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{select_initial_admin_password, write_bootstrap_password_file};

    #[test]
    fn initial_admin_password_prefers_non_empty_configured_value() {
        let (password, generated) = select_initial_admin_password(Some("dev-secret".to_string()));

        assert_eq!(password, "dev-secret");
        assert!(!generated);
    }

    #[test]
    fn initial_admin_password_generates_high_entropy_hex_without_configured_value() {
        let (password, generated) = select_initial_admin_password(None);

        assert!(generated);
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[cfg(unix)]
    #[test]
    fn generated_bootstrap_password_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "restream-bootstrap-password-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        write_bootstrap_password_file(&path, "secret").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);

        assert_eq!(contents, "secret\n");
        assert_eq!(mode, 0o600);
    }
}
