use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineInputRole {
    Primary,
    Backup,
}

impl PipelineInputRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Backup => "backup",
        }
    }
}

impl TryFrom<&str> for PipelineInputRole {
    type Error = PipelineInputRoleParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "primary" => Ok(Self::Primary),
            "backup" => Ok(Self::Backup),
            _ => Err(PipelineInputRoleParseError(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInputRoleParseError(String);

impl fmt::Display for PipelineInputRoleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown pipeline input role: {}", self.0)
    }
}

impl std::error::Error for PipelineInputRoleParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInput {
    pub id: String,
    pub pipeline_id: String,
    pub label: String,
    pub stream_key: String,
    pub role: PipelineInputRole,
    pub enabled: bool,
    pub selected: bool,
    pub source_kind: PipelineInputSourceKind,
    pub source_url: Option<String>,
    pub variant_rule: VariantRule,
    /// How the input authenticates to its origin. A credential: stored and
    /// used, never returned by the API. [`PullAuth::None`] when unset.
    pub source_auth: PullAuth,
}

impl PipelineInput {
    /// Every field at its empty default, with the source half keyed. Use with
    /// struct-update syntax (`PipelineInput { id, ..PipelineInput::keyed_default() }`)
    /// when constructing keyed inputs in tests and fixtures.
    pub fn keyed_default() -> Self {
        Self {
            id: String::new(),
            pipeline_id: String::new(),
            label: String::new(),
            stream_key: String::new(),
            role: PipelineInputRole::Primary,
            enabled: false,
            selected: false,
            source_kind: PipelineInputSourceKind::Keyed,
            source_url: None,
            variant_rule: VariantRule::default(),
            source_auth: PullAuth::None,
        }
    }
}

/// How a pipeline input receives its media.
///
/// `Keyed` inputs wait for an external publisher that presents the input's
/// stream key on one of the ingest listeners; `HlsPull` inputs have no
/// listener at all — the engine fetches a remote HLS origin instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PipelineInputSourceKind {
    #[default]
    Keyed,
    HlsPull,
}

impl PipelineInputSourceKind {
    /// The database representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyed => "keyed",
            Self::HlsPull => "hls_pull",
        }
    }

    /// The protocol label recorded on the ingest session this input opens.
    pub const fn protocol(self) -> &'static str {
        match self {
            Self::Keyed => "keyed",
            Self::HlsPull => "hls-pull",
        }
    }
}

impl TryFrom<&str> for PipelineInputSourceKind {
    type Error = PipelineInputSourceKindParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "keyed" => Ok(Self::Keyed),
            "hls_pull" => Ok(Self::HlsPull),
            _ => Err(PipelineInputSourceKindParseError(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineInputSourceKindParseError(String);

impl fmt::Display for PipelineInputSourceKindParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown pipeline input source kind: {}", self.0)
    }
}

impl std::error::Error for PipelineInputSourceKindParseError {}

/// Which rendition of a remote master playlist an `HlsPull` input follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VariantRule {
    #[default]
    HighestBandwidth,
    LowestBandwidth,
}

impl VariantRule {
    /// The database representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HighestBandwidth => "highest_bandwidth",
            Self::LowestBandwidth => "lowest_bandwidth",
        }
    }
}

impl TryFrom<&str> for VariantRule {
    type Error = VariantRuleParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "highest_bandwidth" => Ok(Self::HighestBandwidth),
            "lowest_bandwidth" => Ok(Self::LowestBandwidth),
            _ => Err(VariantRuleParseError(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantRuleParseError(String);

impl fmt::Display for VariantRuleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown variant rule: {}", self.0)
    }
}

impl std::error::Error for VariantRuleParseError {}

/// One request header an `HlsPull` input presents to its origin.
///
/// Header values are credentials for origins behind a token or cookie, so they
/// are stored and used but never returned by the API.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

impl fmt::Debug for HeaderPair {
    /// The value is a credential; a log line must not carry it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeaderPair")
            .field("name", &self.name)
            .field("value", &crate::secret_display::redact_secret(&self.value))
            .finish()
    }
}

/// How a pull input authenticates to its origin.
///
/// A signed URL needs no entry here: its token is part of the URL. Cookies the
/// origin sets are carried by the fetcher's cookie jar, so they need no entry
/// either. These variants cover what must be sent on every request. All of it
/// is a credential, so only the variant name and a username are ever returned
/// by the API.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PullAuth {
    #[default]
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
    Headers {
        headers: Vec<HeaderPair>,
    },
}

impl fmt::Debug for PullAuth {
    /// Every variant carries a credential, and this type reaches log lines
    /// through the input, the write spec and the pull source, so it prints
    /// the variant and a username and redacts the rest.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |secret: &str| crate::secret_display::redact_secret(secret);
        match self {
            Self::None => formatter.write_str("None"),
            Self::Bearer { token } => formatter
                .debug_struct("Bearer")
                .field("token", &redacted(token))
                .finish(),
            Self::Basic { username, password } => formatter
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &redacted(password))
                .finish(),
            Self::Headers { headers } => formatter
                .debug_struct("Headers")
                .field("headers", headers)
                .finish(),
        }
    }
}

impl PullAuth {
    /// The API's read-side name for the variant, without its secret.
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer { .. } => "bearer",
            Self::Basic { .. } => "basic",
            Self::Headers { .. } => "headers",
        }
    }

    /// The username of a `Basic` entry: shown to the operator because a
    /// username is not a secret. Every other variant has none.
    pub fn username(&self) -> Option<&str> {
        match self {
            Self::Basic { username, .. } => Some(username),
            _ => None,
        }
    }
}

/// The source half of a pipeline input as supplied on create or update.
///
/// This is the write-side view; the read model carries the same facts as flat
/// fields on [`PipelineInput`]. `auth` defaults to [`PullAuth::None`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PipelineInputSource {
    pub kind: PipelineInputSourceKind,
    pub url: Option<String>,
    pub variant_rule: VariantRule,
    pub auth: PullAuth,
}

impl PipelineInputSource {
    /// A keyed input with no remote origin: waits for an external publisher.
    pub fn keyed() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A credential must never reach a log line. These types are printed
    /// through the input row, the write spec and the pull source, so a
    /// derived `Debug` would leak a token, a password or a cookie value into
    /// any `{:?}` — this fails if one is ever re-derived.
    #[test]
    fn debug_prints_no_credential() {
        let token = "Bearer-secret-token";
        let password = "hunter2-secret";
        let cookie = "session=abc123secret";
        let auths = [
            PullAuth::Bearer {
                token: token.to_string(),
            },
            PullAuth::Basic {
                username: "operator".to_string(),
                password: password.to_string(),
            },
            PullAuth::Headers {
                headers: vec![HeaderPair {
                    name: "Cookie".to_string(),
                    value: cookie.to_string(),
                }],
            },
        ];

        for auth in &auths {
            let printed = format!("{auth:?}");
            for secret in [token, password, cookie] {
                assert!(
                    !printed.contains(secret),
                    "{printed} must not carry {secret}"
                );
            }
        }
        // The username is not a secret, and showing it is the ergonomic part.
        assert!(format!("{:?}", auths[1]).contains("operator"));

        let input = PipelineInput {
            id: "input-pull".to_string(),
            pipeline_id: "pipeline-1".to_string(),
            label: "Pull".to_string(),
            stream_key: "pull-key".to_string(),
            role: PipelineInputRole::Primary,
            enabled: true,
            selected: true,
            source_kind: PipelineInputSourceKind::HlsPull,
            source_url: Some("http://origin.example/master.m3u8?token=url-secret".to_string()),
            variant_rule: VariantRule::HighestBandwidth,
            source_auth: auths[0].clone(),
        };
        let printed = format!("{input:?}");
        assert!(
            !printed.contains(token),
            "the input row must redact its auth"
        );
        assert!(printed.contains("input-pull"), "the rest still prints");
    }
}
