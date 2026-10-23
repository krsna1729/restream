//! SQLite adapter for team members, session bindings and the audit log.

use crate::application::ports::*;
use sqlx::SqlitePool;

#[derive(Clone)]
pub struct SqliteTeamStore {
    pool: SqlitePool,
}

impl SqliteTeamStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

fn team_error(err: sqlx::Error) -> TeamStoreError {
    TeamStoreError::new(err.to_string())
}

fn stored_user(row: crate::db::team_repo::UserRow) -> Result<StoredUser, TeamStoreError> {
    let (id, username, role, created_at_ms, disabled, password_hash) = row;
    let role = crate::application::team::Role::parse(&role)
        .ok_or_else(|| TeamStoreError::new(format!("member {id} has unknown role {role:?}")))?;
    Ok(StoredUser {
        user: crate::application::team::UserRecord {
            id,
            username,
            role,
            created_at_ms,
            disabled,
        },
        password_hash,
    })
}

impl TeamStore for SqliteTeamStore {
    fn list_users(&self) -> TeamFuture<'_, Vec<crate::application::team::UserRecord>> {
        Box::pin(async move {
            crate::db::team_repo::list_users(&self.pool)
                .await
                .map_err(team_error)?
                .into_iter()
                .map(|row| stored_user(row).map(|stored| stored.user))
                .collect()
        })
    }

    fn find_user_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> TeamFuture<'a, Option<StoredUser>> {
        Box::pin(async move {
            crate::db::team_repo::find_user_by_username(&self.pool, username)
                .await
                .map_err(team_error)?
                .map(stored_user)
                .transpose()
        })
    }

    fn find_user<'a>(&'a self, id: &'a str) -> TeamFuture<'a, Option<StoredUser>> {
        Box::pin(async move {
            crate::db::team_repo::find_user(&self.pool, id)
                .await
                .map_err(team_error)?
                .map(stored_user)
                .transpose()
        })
    }

    fn insert_user<'a>(
        &'a self,
        user: &'a crate::application::team::UserRecord,
        password_hash: &'a str,
    ) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            crate::db::team_repo::insert_user(
                &self.pool,
                &user.id,
                &user.username,
                user.role.as_str(),
                user.created_at_ms,
                password_hash,
            )
            .await
            .map_err(team_error)
        })
    }

    fn update_user<'a>(
        &'a self,
        id: &'a str,
        role: Option<crate::application::team::Role>,
        disabled: Option<bool>,
        password_hash: Option<&'a str>,
    ) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            crate::db::team_repo::update_user(
                &self.pool,
                id,
                role.map(|role| role.as_str()),
                disabled,
                password_hash,
            )
            .await
            .map_err(team_error)
        })
    }

    fn delete_user<'a>(&'a self, id: &'a str) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            crate::db::team_repo::delete_user(&self.pool, id)
                .await
                .map_err(team_error)
        })
    }

    fn delete_user_sessions<'a>(
        &'a self,
        user_id: &'a str,
        keep: Option<&'a str>,
    ) -> TeamFuture<'a, ()> {
        Box::pin(async move {
            crate::db::team_repo::delete_user_sessions(&self.pool, user_id, keep)
                .await
                .map_err(team_error)
        })
    }

    fn bind_session<'a>(
        &'a self,
        token: &'a str,
        binding: &'a SessionBinding,
    ) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            crate::db::team_repo::set_session_principal(
                &self.pool,
                token,
                binding.user_id.as_deref(),
                binding.view_as.map(|role| role.as_str()),
            )
            .await
            .map_err(team_error)
        })
    }

    fn session_binding<'a>(&'a self, token: &'a str) -> TeamFuture<'a, Option<SessionBinding>> {
        Box::pin(async move {
            let row = crate::db::team_repo::session_principal(&self.pool, token)
                .await
                .map_err(team_error)?;
            Ok(row.map(|(user_id, view_as)| SessionBinding {
                user_id,
                // An unknown stored view narrows to nothing rather than failing the session.
                view_as: view_as
                    .as_deref()
                    .and_then(crate::application::team::Role::parse),
            }))
        })
    }

    fn append_audit<'a>(
        &'a self,
        entry: &'a crate::application::team::AuditEntry,
    ) -> TeamFuture<'a, ()> {
        Box::pin(async move {
            crate::db::team_repo::append_audit(
                &self.pool,
                entry.at_ms,
                &entry.username,
                entry.role.as_str(),
                &entry.method,
                &entry.path,
                entry.status,
            )
            .await
            .map_err(team_error)
        })
    }

    fn list_audit(
        &self,
        limit: u32,
        before: Option<i64>,
    ) -> TeamFuture<'_, Vec<crate::application::team::AuditEntry>> {
        Box::pin(async move {
            let rows = crate::db::team_repo::list_audit(&self.pool, i64::from(limit), before)
                .await
                .map_err(team_error)?;
            Ok(rows
                .into_iter()
                .map(|(id, at_ms, username, role, method, path, status)| {
                    crate::application::team::AuditEntry {
                        id,
                        at_ms,
                        username,
                        role: crate::application::team::Role::parse(&role)
                            .unwrap_or(crate::application::team::Role::Viewer),
                        method,
                        path,
                        status: u16::try_from(status).unwrap_or(0),
                    }
                })
                .collect())
        })
    }
}
