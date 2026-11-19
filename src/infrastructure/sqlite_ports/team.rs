//! SQLite adapter for team members, session bindings and the audit log.

use crate::application::ports::*;
use crate::db::DeadlineDb;

#[derive(Clone)]
pub struct SqliteTeamStore {
    db: DeadlineDb,
}

impl SqliteTeamStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

fn team_error(err: sqlx::Error) -> TeamStoreError {
    TeamStoreError::new(err.to_string())
}

fn stored_user(row: crate::db::team_repo::UserRow) -> Result<StoredUser, TeamStoreError> {
    let (id, username, role, created_at_ms, disabled, password_hash) = row;
    let role = restream_domain::domain::role::Role::parse(&role)
        .ok_or_else(|| TeamStoreError::new(format!("member {id} has unknown role {role:?}")))?;
    Ok(StoredUser {
        user: crate::application::team::UserRecord {
            id,
            username,
            role,
            created_at_ms,
            disabled,
            last_active_ms: None,
        },
        password_hash,
    })
}

impl TeamStore for SqliteTeamStore {
    fn last_active(&self) -> TeamFuture<'_, Vec<(String, i64)>> {
        Box::pin(async move {
            self.db
                .read(|pool| {
                    crate::db::team_repo::last_active(
                        pool,
                        crate::application::services::team_service::BUILTIN_ADMIN_USERNAME,
                    )
                })
                .await
                .map_err(team_error)
        })
    }

    fn list_users(&self) -> TeamFuture<'_, Vec<crate::application::team::UserRecord>> {
        Box::pin(async move {
            self.db
                .read(crate::db::team_repo::list_users)
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
            self.db
                .read(|pool| crate::db::team_repo::find_user_by_username(pool, username))
                .await
                .map_err(team_error)?
                .map(stored_user)
                .transpose()
        })
    }

    fn find_user<'a>(&'a self, id: &'a str) -> TeamFuture<'a, Option<StoredUser>> {
        Box::pin(async move {
            self.db
                .read(|pool| crate::db::team_repo::find_user(pool, id))
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
            let id = user.id.clone();
            let username = user.username.clone();
            let role = user.role;
            let created_at_ms = user.created_at_ms;
            let password_hash = password_hash.to_owned();
            self.db
                .write(move |pool| async move {
                    crate::db::team_repo::insert_user(
                        &pool,
                        &id,
                        &username,
                        role.as_str(),
                        created_at_ms,
                        &password_hash,
                    )
                    .await
                })
                .await
                .map_err(team_error)
        })
    }

    fn update_user<'a>(
        &'a self,
        id: &'a str,
        role: Option<restream_domain::domain::role::Role>,
        disabled: Option<bool>,
        password_hash: Option<&'a str>,
    ) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            let id = id.to_owned();
            let password_hash = password_hash.map(str::to_owned);
            self.db
                .write(move |pool| async move {
                    crate::db::team_repo::update_user(
                        &pool,
                        &id,
                        role.map(|role| role.as_str()),
                        disabled,
                        password_hash.as_deref(),
                    )
                    .await
                })
                .await
                .map_err(team_error)
        })
    }

    fn delete_user<'a>(&'a self, id: &'a str) -> TeamFuture<'a, bool> {
        Box::pin(async move {
            let id = id.to_owned();
            self.db
                .write(
                    move |pool| async move { crate::db::team_repo::delete_user(&pool, &id).await },
                )
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
            let user_id = user_id.to_owned();
            let keep = keep.map(str::to_owned);
            self.db
                .write(move |pool| async move {
                    crate::db::team_repo::delete_user_sessions(&pool, &user_id, keep.as_deref())
                        .await
                })
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
            let token = token.to_owned();
            let user_id = binding.user_id.clone();
            let view_as = binding.view_as;
            self.db
                .write(move |pool| async move {
                    crate::db::team_repo::set_session_principal(
                        &pool,
                        &token,
                        user_id.as_deref(),
                        view_as.map(|role| role.as_str()),
                    )
                    .await
                })
                .await
                .map_err(team_error)
        })
    }

    fn session_binding<'a>(&'a self, token: &'a str) -> TeamFuture<'a, Option<SessionBinding>> {
        Box::pin(async move {
            let row = self
                .db
                .read(|pool| crate::db::team_repo::session_principal(pool, token))
                .await
                .map_err(team_error)?;
            Ok(row.map(|(user_id, view_as)| SessionBinding {
                user_id,
                // An unknown stored view narrows to nothing rather than failing the session.
                view_as: view_as
                    .as_deref()
                    .and_then(restream_domain::domain::role::Role::parse),
            }))
        })
    }

    fn append_audit<'a>(
        &'a self,
        entry: &'a crate::application::team::AuditEntry,
    ) -> TeamFuture<'a, ()> {
        Box::pin(async move {
            let at_ms = entry.at_ms;
            let username = entry.username.clone();
            let role = entry.role;
            let method = entry.method.clone();
            let path = entry.path.clone();
            let status = entry.status;
            self.db
                .write(move |pool| async move {
                    crate::db::team_repo::append_audit(
                        &pool,
                        at_ms,
                        &username,
                        role.as_str(),
                        &method,
                        &path,
                        status,
                    )
                    .await
                })
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
            let rows = self
                .db
                .read(|pool| crate::db::team_repo::list_audit(pool, i64::from(limit), before))
                .await
                .map_err(team_error)?;
            Ok(rows
                .into_iter()
                .map(|(id, at_ms, username, role, method, path, status)| {
                    crate::application::team::AuditEntry {
                        id,
                        at_ms,
                        username,
                        role: restream_domain::domain::role::Role::parse(&role)
                            .unwrap_or(restream_domain::domain::role::Role::Viewer),
                        method,
                        path,
                        status: u16::try_from(status).unwrap_or(0),
                    }
                })
                .collect())
        })
    }
}
