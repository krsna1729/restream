use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
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

#[derive(Clone, PartialEq, Eq)]
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
    /// Inbound HLS push authentication. Only the salted hash is stored.
    pub push_auth: PushAuth,
}

impl fmt::Debug for PipelineInput {
    /// `source_url` is itself a credential when the origin signs its URLs, and
    /// `stream_key` authenticates a push, so both print as a fixed marker;
    /// `source_auth` and `push_auth` redact themselves.
    ///
    /// The domain prints the marker rather than a partly-redacted URL on
    /// purpose. Redacting a URL means deciding which part of it is the
    /// credential, and that decision belongs to the log formatter, which knows
    /// the origin it is talking to (`hdnts`, `Signature` and `X-Amz-Signature`
    /// are queries, and a token can sit in a path segment). The domain keeps
    /// the invariant — no credential, ever, in a `{:?}` — and prints nothing.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PipelineInput")
            .field("id", &self.id)
            .field("pipeline_id", &self.pipeline_id)
            .field("label", &self.label)
            .field("stream_key", &REDACTED)
            .field("role", &self.role)
            .field("enabled", &self.enabled)
            .field("selected", &self.selected)
            .field("source_kind", &self.source_kind)
            .field("source_url", &self.source_url.as_ref().map(|_| REDACTED))
            .field("variant_rule", &self.variant_rule)
            .field("source_auth", &self.source_auth)
            .field("push_auth", &self.push_auth)
            .finish()
    }
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
            push_auth: PushAuth::None,
        }
    }
}

/// How a pipeline input receives its media.
///
/// `Keyed` inputs wait for an external publisher that presents the input's
/// stream key on one of the ingest listeners. `HlsPull` and `SrtCall` inputs
/// have no listener at all: the engine fetches a remote HLS origin, or dials
/// a remote SRT listener (caller mode) and receives its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum PipelineInputSourceKind {
    #[default]
    Keyed,
    HlsPull,
    SrtCall,
}

impl PipelineInputSourceKind {
    /// The database representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyed => "keyed",
            Self::HlsPull => "hls_pull",
            Self::SrtCall => "srt_call",
        }
    }
}

impl TryFrom<&str> for PipelineInputSourceKind {
    type Error = PipelineInputSourceKindParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "keyed" => Ok(Self::Keyed),
            "hls_pull" => Ok(Self::HlsPull),
            "srt_call" => Ok(Self::SrtCall),
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
#[cfg_attr(test, derive(ts_rs::TS))]
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
            .field("value", &REDACTED)
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
    /// A whole cookie string, sent as one `Cookie` header. A signed-cookie
    /// origin (CloudFront's three-part token, for instance) is configured by
    /// pasting what the player would send, rather than by typing a header row.
    Cookies {
        cookies: String,
    },
    Basic {
        username: String,
        password: String,
    },
    Headers {
        headers: Vec<HeaderPair>,
    },
}

/// What a credential prints as. The domain prints one fixed marker for every
/// secret: a prefix and a suffix would be half of a short password, cookie or
/// header value, and a URL's host and path are not safe to print either when
/// the origin signs its URLs.
const REDACTED: &str = "<redacted>";

impl fmt::Debug for PullAuth {
    /// Every variant carries a credential, and this type reaches log lines
    /// through the input, the write spec and the pull source. Only the variant
    /// and a username are printed.
    ///
    /// Every secret prints as the same fixed marker, including a bearer token.
    /// Showing a token's ends would help an operator tell two tokens apart,
    /// but it is still half of a short token, and the domain does not decide
    /// what part of a secret is safe to print.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Bearer { .. } => formatter
                .debug_struct("Bearer")
                .field("token", &REDACTED)
                .finish(),
            Self::Cookies { .. } => formatter
                .debug_struct("Cookies")
                .field("cookies", &REDACTED)
                .finish(),
            Self::Basic { username, .. } => formatter
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &REDACTED)
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
            Self::Cookies { .. } => "cookies",
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

/// How an external HLS publisher authenticates to a keyed input.
///
/// This is stored credential state, never an API response or a plaintext password.
#[derive(Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PushAuth {
    #[default]
    None,
    Basic {
        username: String,
        password_hash: String,
    },
}

impl fmt::Debug for PushAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Basic { username, .. } => formatter
                .debug_struct("Basic")
                .field("username", username)
                .field("password_hash", &REDACTED)
                .finish(),
        }
    }
}

impl PushAuth {
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic { .. } => "basic",
        }
    }

    pub fn username(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Basic { username, .. } => Some(username),
        }
    }
}

/// The source half of a pipeline input as supplied on create or update.
///
/// This is the write-side view; the read model carries the same facts as flat
/// fields on [`PipelineInput`]. Both credential fields default to no auth.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct PipelineInputSource {
    pub kind: PipelineInputSourceKind,
    pub url: Option<String>,
    pub variant_rule: VariantRule,
    pub auth: PullAuth,
    pub push_auth: PushAuth,
}

impl fmt::Debug for PipelineInputSource {
    /// Same rule as [`PipelineInput`]: a signed URL is a credential, so it
    /// prints as a fixed marker, and `auth` redacts itself.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PipelineInputSource")
            .field("kind", &self.kind)
            .field("url", &self.url.as_ref().map(|_| REDACTED))
            .field("variant_rule", &self.variant_rule)
            .field("auth", &self.auth)
            .field("push_auth", &self.push_auth)
            .finish()
    }
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

    #[test]
    fn push_auth_debug_redacts_stored_hash_everywhere() {
        let secret = "salt:secret-derived-password-hash";
        let auth = PushAuth::Basic {
            username: "publisher".to_string(),
            password_hash: secret.to_string(),
        };
        assert_eq!(auth.kind_str(), "basic");
        assert_eq!(auth.username(), Some("publisher"));
        let input = PipelineInput {
            push_auth: auth.clone(),
            ..PipelineInput::keyed_default()
        };
        let source = PipelineInputSource {
            push_auth: auth.clone(),
            ..PipelineInputSource::keyed()
        };
        for printed in [
            format!("{auth:?}"),
            format!("{input:?}"),
            format!("{source:?}"),
        ] {
            assert!(!printed.contains(secret));
            assert!(printed.contains("<redacted>"));
            assert!(printed.contains("publisher"));
        }
        assert_eq!(PushAuth::default().kind_str(), "none");
        assert_eq!(PushAuth::default().username(), None);
    }

    /// A credential must never reach a log line. These types are printed
    /// through the input row, the write spec and the pull source, so a
    /// derived `Debug` would leak a token, a password or a cookie value into
    /// any `{:?}` — this fails if one is ever re-derived.
    #[test]
    fn debug_prints_no_credential() {
        let token = "Bearer-secret-token";
        let password = "hunter2-secret";
        let cookie = "session=abc123secret";
        let url_secret = "url-secret";
        let auths = [
            PullAuth::Bearer {
                token: token.to_string(),
            },
            PullAuth::Cookies {
                cookies: cookie.to_string(),
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
                    "{printed} must not carry a credential"
                );
            }
        }
        // The username is not a secret, and showing it is the ergonomic part.
        assert!(format!("{:?}", auths[2]).contains("operator"));
        // Every secret prints as the same fixed marker. A prefix and a suffix
        // would be half of a short password or cookie, and the domain does not
        // decide what part of a secret is safe to print.
        for auth in &auths {
            assert!(
                format!("{auth:?}").contains("<redacted>"),
                "{auth:?} prints a fixed marker, not a partial secret"
            );
        }

        let input = PipelineInput {
            id: "input-pull".to_string(),
            pipeline_id: "pipeline-1".to_string(),
            label: "Pull".to_string(),
            stream_key: "pull-key".to_string(),
            role: PipelineInputRole::Primary,
            enabled: true,
            selected: true,
            source_kind: PipelineInputSourceKind::HlsPull,
            source_url: Some(format!(
                "http://origin.example/master.m3u8?token={url_secret}"
            )),
            variant_rule: VariantRule::HighestBandwidth,
            source_auth: auths[0].clone(),
            push_auth: PushAuth::None,
        };
        let printed = format!("{input:?}");
        assert!(
            !printed.contains(token),
            "the input row must redact its auth"
        );
        // The stream key authenticates a push, so it is a credential too: the
        // rest of the code prints it through `redact_secret`.
        assert!(
            !printed.contains("pull-key"),
            "the input row must redact its stream key: {printed}"
        );
        // A signed URL is a credential: its query carries the token.
        assert!(
            !printed.contains(url_secret),
            "the input row must redact its source URL: {printed}"
        );
        // The whole URL prints as the marker, host included: the domain does
        // not decide which part of a URL is safe to print.
        assert!(
            printed.contains("<redacted>"),
            "the input row prints a fixed marker for its source URL: {printed}"
        );
        assert!(
            !printed.contains("origin.example"),
            "no part of the source URL prints: {printed}"
        );
        assert!(printed.contains("input-pull"), "the rest still prints");

        let source = PipelineInputSource {
            kind: PipelineInputSourceKind::HlsPull,
            url: Some(format!(
                "http://origin.example/master.m3u8?token={url_secret}"
            )),
            variant_rule: VariantRule::HighestBandwidth,
            auth: auths[1].clone(),
            push_auth: PushAuth::None,
        };
        let printed = format!("{source:?}");
        assert!(
            !printed.contains(url_secret) && !printed.contains(cookie),
            "the write spec redacts its URL and its auth: {printed}"
        );
    }
}
