//! Team members: who may see and change what. Pure: the route-to-role
//! policy, the effective role of a session that views the dashboard as a
//! lower role, and the records the user store exchanges. The roles
//! themselves are [`Role`] (`restream_domain::domain::role`).

use restream_domain::domain::role::Role;
use serde::Serialize;

/// A stored team member, without the password hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRecord {
    pub id: String,
    pub username: String,
    pub role: Role,
    pub created_at_ms: i64,
    pub disabled: bool,
    /// When the member last signed in or made an audited change; `None`
    /// when neither is kept (a list read fills it, a single read does not).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_active_ms: Option<i64>,
}

/// Who a session is, and the role it acts with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub user_id: String,
    pub username: String,
    pub role: Role,
    /// A lower role the session chose to see the dashboard as.
    pub view_as: Option<Role>,
}

impl Principal {
    /// The role every check uses: the chosen view, never above the user's own.
    pub fn effective_role(&self) -> Role {
        self.view_as.map_or(self.role, |view| view.min(self.role))
    }
}

/// One audited change: who, as which role, what, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: i64,
    pub at_ms: i64,
    pub username: String,
    pub role: Role,
    pub method: String,
    pub path: String,
    pub status: u16,
}

/// Share links carry the token that opens a stream to anyone.
fn is_share_links(path: &str) -> bool {
    path.starts_with("/api/v1/share-links")
        || (path.starts_with("/api/v1/pipelines/") && path.ends_with("/share-links"))
}

/// Paths whose reads expose secrets (stream keys, destination URLs,
/// notification targets) or the team itself.
fn read_role(path: &str) -> Role {
    if path.starts_with("/api/v1/notifications/push/") {
        Role::Viewer
    } else if path.starts_with("/api/v1/users")
        || path.starts_with("/api/v1/notifications")
        || is_share_links(path)
        || path.starts_with("/api/v1/audit")
        || path.starts_with("/api/v1/security/")
    {
        Role::Admin
    } else if path == "/api/v1/settings" || path.starts_with("/api/v1/stream-keys") {
        Role::Operator
    } else {
        Role::Viewer
    }
}

/// The least role a request needs. Reads need a viewer (except the secret
/// and team reads above); a session's own auth actions and push
/// subscriptions need only a session; settings, security, custom encodings,
/// notification channels and the team need an admin; every other change
/// needs an operator.
pub fn required_role(method: &str, path: &str) -> Role {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return read_role(path);
    }
    // A session's own auth actions and a member's own browser push
    // subscriptions need only a session.
    if path.starts_with("/api/v1/auth/") || path.starts_with("/api/v1/notifications/push/") {
        return Role::Viewer;
    }
    if path == "/api/v1/settings"
        || is_share_links(path)
        || path.starts_with("/api/v1/notifications")
        || path.starts_with("/api/v1/security/")
        || path.starts_with("/api/v1/users")
        || path.starts_with("/api/v1/encodings/custom")
    {
        return Role::Admin;
    }
    Role::Operator
}

/// Usernames: 1-64 bytes of letters, digits, `.`, `_`, `-` or `@`.
pub fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewing_as_narrows_and_never_widens() {
        let mut principal = Principal {
            user_id: "u".into(),
            username: "op".into(),
            role: Role::Operator,
            view_as: None,
        };
        assert_eq!(principal.effective_role(), Role::Operator);
        principal.view_as = Some(Role::Viewer);
        assert_eq!(principal.effective_role(), Role::Viewer);
        principal.view_as = Some(Role::Admin);
        assert_eq!(
            principal.effective_role(),
            Role::Operator,
            "a view never grants more"
        );
    }

    #[test]
    fn the_route_policy_by_method_and_path() {
        let cases = [
            ("GET", "/api/v1/state", Role::Viewer),
            ("GET", "/api/v1/pipelines/p/flight-recorder", Role::Viewer),
            ("GET", "/hls/p/master.m3u8", Role::Viewer),
            ("GET", "/api/v1/settings", Role::Operator),
            ("GET", "/api/v1/stream-keys", Role::Operator),
            ("GET", "/api/v1/users", Role::Admin),
            ("GET", "/api/v1/audit", Role::Admin),
            ("GET", "/api/v1/security/rate-limits", Role::Admin),
            ("POST", "/api/v1/auth/view-as", Role::Viewer),
            ("POST", "/api/v1/auth/logout", Role::Viewer),
            (
                "POST",
                "/api/v1/pipelines/p/outputs/o/start",
                Role::Operator,
            ),
            ("PATCH", "/api/v1/pipelines/p", Role::Operator),
            ("DELETE", "/api/v1/media/a.mp4", Role::Operator),
            ("PATCH", "/api/v1/settings", Role::Admin),
            ("POST", "/api/v1/security/rate-limits/reset", Role::Admin),
            ("POST", "/api/v1/users", Role::Admin),
            ("DELETE", "/api/v1/users/u", Role::Admin),
            ("PUT", "/api/v1/encodings/custom", Role::Admin),
            ("GET", "/api/v1/notifications/channels", Role::Admin),
            ("POST", "/api/v1/notifications/channels/c/test", Role::Admin),
            ("GET", "/api/v1/notifications/deliveries", Role::Admin),
            ("GET", "/api/v1/notifications/push/key", Role::Viewer),
            (
                "POST",
                "/api/v1/notifications/push/subscriptions",
                Role::Viewer,
            ),
            (
                "DELETE",
                "/api/v1/notifications/push/subscriptions/s",
                Role::Viewer,
            ),
        ];
        for (method, path, role) in cases {
            assert_eq!(required_role(method, path), role, "{method} {path}");
        }
    }

    #[test]
    fn usernames_are_bounded_and_plain() {
        assert!(valid_username("ops.lead@example"));
        assert!(!valid_username(""));
        assert!(!valid_username("has space"));
        assert!(!valid_username(&"a".repeat(65)));
    }
}
