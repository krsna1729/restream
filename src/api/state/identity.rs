//! Dashboard identity operations: built-in auth state, user records, sessions,
//! and the audit trail.

use super::AppState;
use crate::application::services::team_service::StoredUser;
use crate::application::team::{AuditEntry, Principal, UserRecord};
use restream_domain::domain::role::Role;

impl AppState {
    /// Persists one auth-specific meta entry.
    pub(crate) async fn set_auth_meta(
        &self,
        key: &str,
        value: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.auth_service.set_meta(key, value).await
    }

    /// Returns the built-in dashboard password hash.
    pub(crate) async fn password_hash(
        &self,
    ) -> crate::application::services::ServiceResult<Option<String>> {
        self.auth_service.get_password_hash().await
    }

    /// Creates one persisted dashboard session.
    pub(crate) async fn create_session(
        &self,
        token_hash: &str,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<()> {
        self.auth_service.create_session(token_hash, now_ms).await
    }

    /// Deletes one persisted dashboard session.
    pub(crate) async fn delete_session(
        &self,
        token_hash: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.auth_service.delete_session(token_hash).await
    }

    /// Stores a new built-in dashboard password hash.
    pub(crate) async fn set_password_hash(
        &self,
        hash: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.auth_service.set_password_hash(hash).await
    }

    /// Deletes every dashboard session except the caller's.
    pub(crate) async fn delete_sessions_except(
        &self,
        token_hash: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.auth_service.delete_sessions_except(token_hash).await
    }

    /// Looks up a login candidate by username.
    pub(crate) async fn login_candidate(
        &self,
        username: &str,
    ) -> crate::application::services::ServiceResult<Option<StoredUser>> {
        self.team_service.login_candidate(username).await
    }

    /// Binds one session to a member, or to the built-in admin when absent.
    pub(crate) async fn bind_session(
        &self,
        token_hash: &str,
        user_id: Option<&str>,
    ) -> crate::application::services::ServiceResult<()> {
        self.team_service.bind_session(token_hash, user_id).await
    }

    /// Resolves one session's principal.
    pub(crate) async fn principal(
        &self,
        token_hash: &str,
    ) -> crate::application::services::ServiceResult<Option<Principal>> {
        self.team_service.principal(token_hash).await
    }

    /// Returns one member's stored record and password hash.
    pub(crate) async fn stored_user(
        &self,
        user_id: &str,
    ) -> crate::application::services::ServiceResult<Option<StoredUser>> {
        self.team_service.stored_user(user_id).await
    }

    /// Changes one member's own password and rotates their sessions.
    pub(crate) async fn set_own_password(
        &self,
        user_id: &str,
        password_hash: &str,
        token_hash: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.team_service
            .set_own_password(user_id, password_hash, token_hash)
            .await
    }

    /// Changes one session's view-as role.
    pub(crate) async fn set_view_as(
        &self,
        token_hash: &str,
        principal: &Principal,
        view_as: Option<Role>,
    ) -> crate::application::services::ServiceResult<Principal> {
        self.team_service
            .set_view_as(token_hash, principal, view_as)
            .await
    }

    /// Appends one audit entry.
    pub(crate) async fn record_audit(
        &self,
        entry: &AuditEntry,
    ) -> crate::application::services::ServiceResult<()> {
        self.team_service.record(entry).await
    }

    /// Lists users.
    pub(crate) async fn list_users(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<UserRecord>> {
        self.team_service.list_users().await
    }

    /// Creates one user with a password hash.
    pub(crate) async fn create_user(
        &self,
        user: UserRecord,
        password_hash: &str,
    ) -> crate::application::services::ServiceResult<UserRecord> {
        self.team_service.create_user(user, password_hash).await
    }

    /// Updates one user's role, disabled flag, or password hash.
    pub(crate) async fn update_user(
        &self,
        id: &str,
        role: Option<Role>,
        disabled: Option<bool>,
        password_hash: Option<&str>,
    ) -> crate::application::services::ServiceResult<UserRecord> {
        self.team_service
            .update_user(id, role, disabled, password_hash)
            .await
    }

    /// Deletes one user.
    pub(crate) async fn delete_user(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.team_service.delete_user(id).await
    }

    /// Lists audit entries newest first.
    #[doc(hidden)]
    pub async fn audit_entries(
        &self,
        limit: u32,
        before: Option<i64>,
    ) -> crate::application::services::ServiceResult<Vec<AuditEntry>> {
        self.team_service.audit(limit, before).await
    }
}
