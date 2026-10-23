//! Team members and the sessions they act through.
//!
//! The built-in `admin` is not a stored member: its password stays in the
//! dashboard password record, and a session bound to no member is its. Every
//! other member is stored with a role; a session resolves to a [`Principal`]
//! on each checked request, so a role change or a disable takes effect at
//! once.

use std::sync::Arc;

use crate::application::ports::{SessionBinding, StoredUser, TeamStore};
use crate::application::team::{AuditEntry, Principal, Role, UserRecord, valid_username};

use super::error::{ServiceError, ServiceResult};

pub const BUILTIN_ADMIN_ID: &str = "admin";
pub const BUILTIN_ADMIN_USERNAME: &str = "admin";

fn builtin_admin() -> UserRecord {
    UserRecord {
        id: BUILTIN_ADMIN_ID.to_string(),
        username: BUILTIN_ADMIN_USERNAME.to_string(),
        role: Role::Admin,
        created_at_ms: 0,
        disabled: false,
    }
}

fn store_error(
    context: &str,
) -> impl Fn(crate::application::ports::TeamStoreError) -> ServiceError + '_ {
    move |err| ServiceError::internal(format!("{context}: {err}"))
}

#[derive(Clone)]
pub struct TeamService {
    store: Arc<dyn TeamStore>,
}

impl TeamService {
    pub fn with_store(store: Arc<dyn TeamStore>) -> Self {
        Self { store }
    }

    /// The member behind a session (by token hash), or `None` when the
    /// session or its member is gone or disabled.
    pub async fn principal(&self, token_hash: &str) -> ServiceResult<Option<Principal>> {
        let Some(binding) = self
            .store
            .session_binding(token_hash)
            .await
            .map_err(store_error("read session"))?
        else {
            return Ok(None);
        };
        let user = match binding.user_id {
            None => builtin_admin(),
            Some(id) => match self
                .store
                .find_user(&id)
                .await
                .map_err(store_error("read member"))?
            {
                Some(stored) if !stored.user.disabled => stored.user,
                _ => return Ok(None),
            },
        };
        Ok(Some(Principal {
            user_id: user.id,
            username: user.username,
            role: user.role,
            view_as: binding.view_as,
        }))
    }

    /// The stored member a login names, unless disabled. The built-in admin
    /// is not here: its password is the dashboard password.
    pub async fn login_candidate(&self, username: &str) -> ServiceResult<Option<StoredUser>> {
        Ok(self
            .store
            .find_user_by_username(username)
            .await
            .map_err(store_error("read member"))?
            .filter(|stored| !stored.user.disabled))
    }

    pub async fn stored_user(&self, id: &str) -> ServiceResult<Option<StoredUser>> {
        self.store
            .find_user(id)
            .await
            .map_err(store_error("read member"))
    }

    /// Binds a fresh session to the member who logged in.
    pub async fn bind_session(&self, token_hash: &str, user_id: Option<&str>) -> ServiceResult<()> {
        let binding = SessionBinding {
            user_id: user_id.map(str::to_string),
            view_as: None,
        };
        self.store
            .bind_session(token_hash, &binding)
            .await
            .map_err(store_error("bind session"))?;
        Ok(())
    }

    /// Views the dashboard as a lower role, or as the member's own (`None`).
    /// Asking for the member's own role or above is the same as `None`.
    pub async fn set_view_as(
        &self,
        token_hash: &str,
        principal: &Principal,
        view_as: Option<Role>,
    ) -> ServiceResult<Principal> {
        let view_as = view_as.filter(|view| *view < principal.role);
        let binding = SessionBinding {
            user_id: (principal.user_id != BUILTIN_ADMIN_ID).then(|| principal.user_id.clone()),
            view_as,
        };
        if !self
            .store
            .bind_session(token_hash, &binding)
            .await
            .map_err(store_error("bind session"))?
        {
            return Err(ServiceError::not_found("session ended"));
        }
        Ok(Principal {
            view_as,
            ..principal.clone()
        })
    }

    /// The built-in admin first, then the stored members.
    pub async fn list_users(&self) -> ServiceResult<Vec<UserRecord>> {
        let mut users = vec![builtin_admin()];
        users.extend(
            self.store
                .list_users()
                .await
                .map_err(store_error("list members"))?,
        );
        Ok(users)
    }

    pub async fn create_user(
        &self,
        user: UserRecord,
        password_hash: &str,
    ) -> ServiceResult<UserRecord> {
        if !valid_username(&user.username) {
            return Err(ServiceError::conflict(
                "A username is 1-64 letters, digits, '.', '_', '-' or '@'.",
            ));
        }
        if user.username == BUILTIN_ADMIN_USERNAME || user.id == BUILTIN_ADMIN_ID {
            return Err(ServiceError::conflict(
                "The username admin is the built-in admin's.",
            ));
        }
        if !self
            .store
            .insert_user(&user, password_hash)
            .await
            .map_err(store_error("create member"))?
        {
            return Err(ServiceError::conflict(format!(
                "{} is already a member.",
                user.username
            )));
        }
        Ok(user)
    }

    /// Changes a member's role, enabled state or password. A disabled member's
    /// sessions end; a role change applies to their next request.
    pub async fn update_user(
        &self,
        id: &str,
        role: Option<Role>,
        disabled: Option<bool>,
        password_hash: Option<&str>,
    ) -> ServiceResult<UserRecord> {
        if id == BUILTIN_ADMIN_ID {
            return Err(ServiceError::conflict(
                "The built-in admin is always an enabled admin; change its password under Security.",
            ));
        }
        if !self
            .store
            .update_user(id, role, disabled, password_hash)
            .await
            .map_err(store_error("update member"))?
        {
            return Err(ServiceError::not_found(format!("no member {id}")));
        }
        if disabled == Some(true) || password_hash.is_some() {
            self.end_sessions(id, None).await?;
        }
        self.stored_user(id)
            .await?
            .map(|stored| stored.user)
            .ok_or_else(|| ServiceError::not_found(format!("no member {id}")))
    }

    /// A member's own password change: their other sessions end, this one
    /// (`keep`, a token hash) stays.
    pub async fn set_own_password(
        &self,
        id: &str,
        password_hash: &str,
        keep: &str,
    ) -> ServiceResult<()> {
        if !self
            .store
            .update_user(id, None, None, Some(password_hash))
            .await
            .map_err(store_error("update member"))?
        {
            return Err(ServiceError::not_found(format!("no member {id}")));
        }
        self.end_sessions(id, Some(keep)).await
    }

    pub async fn delete_user(&self, id: &str) -> ServiceResult<()> {
        if id == BUILTIN_ADMIN_ID {
            return Err(ServiceError::conflict(
                "The built-in admin cannot be removed.",
            ));
        }
        self.end_sessions(id, None).await?;
        if !self
            .store
            .delete_user(id)
            .await
            .map_err(store_error("delete member"))?
        {
            return Err(ServiceError::not_found(format!("no member {id}")));
        }
        Ok(())
    }

    /// Ends a member's sessions except `keep` (a token hash).
    pub async fn end_sessions(&self, user_id: &str, keep: Option<&str>) -> ServiceResult<()> {
        self.store
            .delete_user_sessions(user_id, keep)
            .await
            .map_err(store_error("end member sessions"))
    }

    pub async fn record(&self, entry: &AuditEntry) -> ServiceResult<()> {
        self.store
            .append_audit(entry)
            .await
            .map_err(store_error("append audit"))
    }

    pub async fn audit(&self, limit: u32, before: Option<i64>) -> ServiceResult<Vec<AuditEntry>> {
        self.store
            .list_audit(limit.clamp(1, 500), before)
            .await
            .map_err(store_error("read audit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::service_wiring::SqliteServiceFactory;

    async fn service() -> (TeamService, sqlx::SqlitePool) {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        (SqliteServiceFactory::new(&pool).team_service(), pool)
    }

    async fn session(pool: &sqlx::SqlitePool, token: &str) {
        crate::db::create_session(pool, token, 1).await.unwrap();
    }

    fn member(id: &str, username: &str, role: Role) -> UserRecord {
        UserRecord {
            id: id.into(),
            username: username.into(),
            role,
            created_at_ms: 5,
            disabled: false,
        }
    }

    #[tokio::test]
    async fn an_unbound_session_is_the_builtin_admin() {
        let (team, pool) = service().await;
        assert_eq!(
            team.principal("t").await.unwrap(),
            None,
            "no session, no principal"
        );
        session(&pool, "t").await;
        let principal = team.principal("t").await.unwrap().unwrap();
        assert_eq!(
            (principal.username.as_str(), principal.role),
            ("admin", Role::Admin)
        );
    }

    #[tokio::test]
    async fn a_member_session_carries_their_role_and_ends_when_disabled() {
        let (team, pool) = service().await;
        team.create_user(member("u1", "ops", Role::Operator), "h")
            .await
            .unwrap();
        session(&pool, "t").await;
        team.bind_session("t", Some("u1")).await.unwrap();
        let principal = team.principal("t").await.unwrap().unwrap();
        assert_eq!(principal.effective_role(), Role::Operator);

        team.update_user("u1", Some(Role::Viewer), None, None)
            .await
            .unwrap();
        assert_eq!(
            team.principal("t").await.unwrap().unwrap().role,
            Role::Viewer,
            "role applies at once"
        );

        team.update_user("u1", None, Some(true), None)
            .await
            .unwrap();
        assert_eq!(
            team.principal("t").await.unwrap(),
            None,
            "disabled: the session is gone"
        );
        assert_eq!(
            crate::db::get_session_created_at(&pool, "t").await.unwrap(),
            None
        );
        assert!(
            team.login_candidate("ops").await.unwrap().is_none(),
            "and cannot log in"
        );
    }

    #[tokio::test]
    async fn view_as_narrows_the_session_and_survives_lookup() {
        let (team, pool) = service().await;
        session(&pool, "t").await;
        let admin = team.principal("t").await.unwrap().unwrap();
        let viewing = team
            .set_view_as("t", &admin, Some(Role::Viewer))
            .await
            .unwrap();
        assert_eq!(viewing.effective_role(), Role::Viewer);
        let reread = team.principal("t").await.unwrap().unwrap();
        assert_eq!(
            (reread.role, reread.effective_role()),
            (Role::Admin, Role::Viewer)
        );
        // Asking for one's own role exits the view.
        let back = team
            .set_view_as("t", &reread, Some(Role::Admin))
            .await
            .unwrap();
        assert_eq!(back.view_as, None);
        assert_eq!(
            team.principal("t").await.unwrap().unwrap().effective_role(),
            Role::Admin
        );
    }

    #[tokio::test]
    async fn the_builtin_admin_and_usernames_are_protected() {
        let (team, _pool) = service().await;
        assert!(matches!(
            team.create_user(member("x", "admin", Role::Viewer), "h")
                .await,
            Err(ServiceError::Conflict(_))
        ));
        assert!(matches!(
            team.create_user(member("x", "bad name", Role::Viewer), "h")
                .await,
            Err(ServiceError::Conflict(_))
        ));
        team.create_user(member("a", "ann", Role::Viewer), "h")
            .await
            .unwrap();
        assert!(matches!(
            team.create_user(member("b", "ann", Role::Viewer), "h")
                .await,
            Err(ServiceError::Conflict(_))
        ));
        assert!(matches!(
            team.delete_user(BUILTIN_ADMIN_ID).await,
            Err(ServiceError::Conflict(_))
        ));
        assert!(matches!(
            team.update_user(BUILTIN_ADMIN_ID, Some(Role::Viewer), None, None)
                .await,
            Err(ServiceError::Conflict(_))
        ));
        let names: Vec<_> = team
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .map(|u| u.username)
            .collect();
        assert_eq!(names, ["admin", "ann"]);
    }

    #[tokio::test]
    async fn the_audit_log_pages_newest_first() {
        let (team, _pool) = service().await;
        for index in 0..5 {
            team.record(&AuditEntry {
                id: 0,
                at_ms: index,
                username: "ops".into(),
                role: Role::Operator,
                method: "POST".into(),
                path: format!("/api/v1/x/{index}"),
                status: 200,
            })
            .await
            .unwrap();
        }
        let page = team.audit(2, None).await.unwrap();
        assert_eq!(page.iter().map(|e| e.at_ms).collect::<Vec<_>>(), [4, 3]);
        let next = team.audit(2, Some(page[1].id)).await.unwrap();
        assert_eq!(next.iter().map(|e| e.at_ms).collect::<Vec<_>>(), [2, 1]);
    }
}
