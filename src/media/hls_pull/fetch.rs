//! The HTTP layer of one pull: the client, the requests it sends, and the
//! credentials that ride them.
//!
//! Two guards keep credentials from leaving the source's origin. [`AuthScope`]
//! presents them only on requests that share the origin, and the client's
//! redirect policy refuses to follow a redirect out of it — reqwest's own
//! guard strips `Authorization`, `Cookie` and `Proxy-Authorization` on a
//! cross-host redirect but leaves a custom header such as `X-Api-Key` on the
//! request. [`host_refused`] is the third: it refuses a playlist's or a
//! redirect's hop into loopback, link-local (cloud metadata) or a private
//! range, unless the source itself is there.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Url, redirect};
use tracing::warn;
use url::Host;

use crate::domain::pipeline_input::{HeaderPair, PullAuth};
use crate::media::hls_ingest::{BodyError, ObjectBody};

/// Connect timeout for origin requests; a stalled request is bounded by
/// `REQUEST_TIMEOUT` and by task cancellation.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Redirect hops a pull follows, as reqwest's default policy allows. A custom
/// policy has to bound the chain itself.
const MAX_REDIRECTS: usize = 10;
const USER_AGENT: &str = concat!("Restream/", env!("CARGO_PKG_VERSION"));

/// A pull's origin credentials, validated once.
pub(super) enum RequestAuth {
    None,
    Bearer(String),
    /// A whole cookie string, sent as the one `Cookie` header.
    Cookies(HeaderValue),
    Basic {
        username: String,
        password: String,
    },
    Headers(HeaderMap),
}

impl RequestAuth {
    pub(super) fn new(auth: &PullAuth) -> Result<Self, String> {
        Ok(match auth {
            PullAuth::None => Self::None,
            PullAuth::Bearer { token } => {
                // An invalid token would only fail at send time; reject it once.
                HeaderValue::from_str(token).map_err(|_| "invalid bearer token".to_string())?;
                Self::Bearer(token.clone())
            }
            PullAuth::Cookies { cookies } => {
                // The API validates a cookie string as a header value, so this
                // cannot fail for a saved input; a value that still cannot be
                // built must not reach a request.
                Self::Cookies(
                    HeaderValue::from_str(cookies)
                        .map_err(|_| "invalid cookie string".to_string())?,
                )
            }
            PullAuth::Basic { username, password } => Self::Basic {
                username: username.clone(),
                password: password.clone(),
            },
            PullAuth::Headers { headers } => Self::Headers(pair_headers(headers)?),
        })
    }

    /// Whether this pull has a credential to protect at all.
    pub(super) fn is_configured(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub(super) fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::None => builder,
            Self::Bearer(token) => builder.bearer_auth(token),
            // One header, as a player sends it. The jar's own cookies are
            // added beside it for their domain, as reqwest merges them.
            Self::Cookies(cookies) => builder.header(header::COOKIE, cookies.clone()),
            Self::Basic { username, password } => builder.basic_auth(username, Some(password)),
            Self::Headers(headers) => builder.headers(headers.clone()),
        }
    }
}

/// An origin as RFC 6454 defines it: scheme, host and port. A URL with no
/// host or no known port has no origin and matches nothing.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    pub(super) fn of(url: &Url) -> Option<Self> {
        Some(Self {
            scheme: url.scheme().to_string(),
            host: url.host_str()?.to_ascii_lowercase(),
            port: url.port_or_known_default()?,
        })
    }
}

/// The credentials of one pull, and the one origin they belong to. The
/// playlist URIs are resolved against the playlist URL but kept when they are
/// absolute, so a segment, map or variant on another host — an SSAI ad
/// segment, a second CDN, a compromised edge — would otherwise receive the
/// token or the password with no redirect involved. Only requests to the
/// source's own origin get any credential; every other origin gets none.
/// An `https` source whose playlist lists `http://` URIs on the same host and
/// port is another origin too (the scheme differs), so it gets none either.
pub(super) struct AuthScope {
    origin: Option<Origin>,
    auth: RequestAuth,
}

impl AuthScope {
    pub(super) fn new(source: &Url, auth: RequestAuth) -> Self {
        Self {
            origin: Origin::of(source),
            auth,
        }
    }

    /// Whether this pull has a credential to protect, which is what makes a
    /// downgrade or a redirect out of the origin worth refusing.
    pub(super) fn is_configured(&self) -> bool {
        self.auth.is_configured()
    }

    pub(super) fn apply(
        &self,
        url: &Url,
        builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match (&self.origin, Origin::of(url)) {
            (Some(origin), Some(target)) if *origin == target => self.auth.apply(builder),
            _ => builder,
        }
    }
}

/// Whether `url`'s host is in an address range a pull must not reach on its
/// own: loopback, link-local (cloud metadata lives at 169.254.169.254,
/// 100.100.100.200, fd00:ec2::254 and their NAT64 forms), carrier-grade NAT,
/// the private and unique-local ranges, and the `localhost` names.
///
/// The host comes from [`Url::host`], never from `host_str`: `host_str`
/// returns an IPv6 literal *bracketed* (`"[::1]"`), which parses as no address
/// at all, so a string guard would let every IPv6 loopback, link-local,
/// unique-local and v4-mapped target through.
///
/// A literal is what is decided here: a name that *resolves* to one of these
/// ranges (`ssrf.example` pointing at 169.254.169.254) is the resolver's
/// business and gets through this guard.
pub(super) fn host_refused(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => ipv4_refused(address),
        Some(Host::Ipv6(address)) => match embedded_ipv4(address) {
            // ::ffff:a.b.c.d and the NAT64 prefix 64:ff9b::/96 both carry an
            // IPv4 address: the metadata endpoint can be reached that way
            // round too.
            Some(embedded) => ipv4_refused(embedded),
            None => {
                let octets = address.octets();
                address.is_loopback()
                    || address.is_unspecified()
                    // fe80::/10, link-local.
                    || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
                    // fc00::/7, unique local (fd00:ec2::254 is metadata too).
                    || (octets[0] & 0xfe) == 0xfc
            }
        },
        Some(Host::Domain(name)) => localhost_name(name),
        None => false,
    }
}

/// The IPv4 address an IPv6 literal carries, when it carries one:
/// `::ffff:0:0/96` (v4-mapped) and `64:ff9b::/96` (the well-known NAT64
/// prefix, whose low 32 bits are the destination).
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return Some(mapped);
    }
    let [first, second, third, fourth, gap @ .., a, b, c, d] = address.octets();
    let is_nat64 = [first, second, third, fourth] == [0x00, 0x64, 0xff, 0x9b]
        && gap.iter().all(|byte| *byte == 0);
    is_nat64.then_some(Ipv4Addr::new(a, b, c, d))
}

/// `localhost` in the forms a URL can carry it (RFC 6761): the name, the name
/// with a trailing dot, and any name under it.
fn localhost_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    name.eq_ignore_ascii_case("localhost")
        || name
            .rsplit_once('.')
            .is_some_and(|(_, label)| label.eq_ignore_ascii_case("localhost"))
}

fn ipv4_refused(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    first == 0
        || first == 10
        || first == 127
        || (first == 169 && second == 254)
        // 172.16.0.0/12 (RFC 1918) and 100.64.0.0/10 (RFC 6598 carrier-grade
        // NAT, where Alibaba Cloud's metadata endpoint 100.100.100.200 lives).
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 168)
        || (first == 100 && (64..=127).contains(&second))
}

/// How a URL is rendered in a pull log line: scheme, host, port and path,
/// with any query replaced by `?<redacted>`.
///
/// [`crate::secret_display::redact_url`] redacts only the query keys it knows
/// (`streamid`, `passphrase`, `key`, `stream_key`, `token`, `cid`), while a
/// signed origin carries its credential under its own key — Akamai `hdnts`,
/// CloudFront `Policy`/`Signature`/`Key-Pair-Id`, S3 `X-Amz-Signature` — which
/// it would print verbatim. The whole query is therefore dropped, at every
/// site that renders a URL: a segment or map URL from a signed playlist
/// carries a signature of its own, and so does a master that hands out signed
/// variant URLs. Userinfo is dropped with it, the same reason the source URL
/// may not carry any.
pub(super) fn log_url(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        // Not a URL: keep the part before any query, which is where a
        // credential would sit.
        return match raw.split_once('?') {
            Some((head, _)) => format!("{head}?<redacted>"),
            None => raw.to_string(),
        };
    };
    let mut rendered = String::with_capacity(raw.len());
    rendered.push_str(url.scheme());
    rendered.push_str("://");
    rendered.push_str(url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        rendered.push(':');
        rendered.push_str(&port.to_string());
    }
    rendered.push_str(url.path());
    if url.query().is_some() {
        rendered.push_str("?<redacted>");
    }
    rendered
}

/// Why a redirect must not be followed, or `None` when the pull may follow
/// it.
///
/// A downgrade is refused for every pull: an `https` source must not be
/// silently weakened into fetching the clear.
///
/// A hop out of the source's origin is refused when the pull has credentials
/// to protect — they are presented per origin, and reqwest's own guard leaves
/// a custom header such as `X-Api-Key` on the request — or when the target
/// fails the address guard. With neither, it is followed, so a credential-less
/// origin that answers `302` to its nearest edge works. `allow_private` is the
/// source's own answer to that guard: a source that is itself in one of those
/// ranges may be redirected within them.
pub(super) fn redirect_refusal(
    previous: &Url,
    next: &Url,
    auth_configured: bool,
    allow_private: bool,
) -> Option<&'static str> {
    if previous.scheme() == "https" && next.scheme() == "http" {
        return Some("redirect would take the pull to http");
    }
    if Origin::of(previous) == Origin::of(next) {
        return None;
    }
    if auth_configured {
        return Some("redirect would carry credentials to another origin");
    }
    if !allow_private && host_refused(next) {
        return Some("redirect points at a private address");
    }
    None
}

/// Whether a failed playlist fetch means the per-variant URL is gone rather
/// than the origin being unhappy: a signed URL that expired answers 403, an
/// edge that dropped the variant answers 404, and a rotated session key
/// answers 401.
pub(super) fn variant_expired(error: &FetchError) -> bool {
    matches!(error, FetchError::Status(401 | 403 | 404))
}

pub(super) fn pair_headers(pairs: &[HeaderPair]) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for pair in pairs {
        let name = HeaderName::from_bytes(pair.name.trim().as_bytes())
            .map_err(|_| format!("invalid request header name {:?}", pair.name))?;
        // The value is a credential: the error names the header, never it.
        let value = HeaderValue::from_str(&pair.value)
            .map_err(|_| format!("invalid value for request header {:?}", pair.name))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

/// The pull's HTTP client. `auth_configured` and `allow_private` are what the
/// redirect policy refuses on; see [`redirect_refusal`].
pub(super) fn build_client(auth_configured: bool, allow_private: bool) -> Client {
    let tls = crate::media::egress::tls::resolve_client_config(None).unwrap_or_else(|error| {
        warn!(%error, "HLS pull TLS configuration unusable; using the default roots");
        crate::media::egress::tls::rustls_client_config()
    });
    Client::builder()
        .user_agent(USER_AGENT)
        // Mechanism 2 (signed cookies): a `Set-Cookie` on a playlist is
        // presented with that playlist's segments, as a browser or FFmpeg
        // `-cookies` does. Cookies are scoped by domain by the jar.
        .cookie_store(true)
        // A redirect is followed inside the hop count, and otherwise only as
        // `redirect_refusal` allows: a downgrade is refused outright, and a
        // hop out of the origin only for a credential-less pull pointed at a
        // host the address guard accepts. Anything else is stopped, and the
        // 3xx is what the caller then sees. Credentials are applied per origin
        // on top of this (see `AuthScope`), so this is the second guard rather
        // than the only one. A custom policy owns its own hop bound.
        .redirect(redirect::Policy::custom(move |attempt| {
            let refused = attempt.previous().len() > MAX_REDIRECTS
                || attempt.previous().last().is_some_and(|previous| {
                    redirect_refusal(previous, attempt.url(), auth_configured, allow_private)
                        .is_some()
                });
            if refused {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .use_preconfigured_tls((*tls).clone())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|error| {
            warn!(%error, "HLS pull client build failed; using a default client");
            Client::new()
        })
}

pub(super) async fn send(
    client: &Client,
    url: &Url,
    scope: &AuthScope,
) -> Result<reqwest::Response, FetchError> {
    let response = scope
        .apply(url, client.get(url.clone()))
        .send()
        .await
        .map_err(|error| FetchError::Request(error.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    Ok(response)
}

/// Read a bounded playlist body.
pub(super) async fn fetch_small(
    client: &Client,
    url: &Url,
    scope: &AuthScope,
    max_bytes: usize,
) -> Result<Vec<u8>, FetchError> {
    let response = send(client, url, scope).await?;
    let declared = response.content_length();
    if declared.is_some_and(|length| length > u64::try_from(max_bytes).unwrap_or(u64::MAX)) {
        return Err(FetchError::TooLarge);
    }
    let capacity = declared
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(max_bytes);
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| FetchError::Body(error.without_url().to_string()))?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Stream a bounded segment body into an ingest body, as an upload handler
/// frames its request body.
pub(super) async fn pump_body(
    response: reqwest::Response,
    limit: usize,
    body: &mut ObjectBody,
) -> Result<(), FetchError> {
    if let Some(length) = response.content_length()
        && length > u64::try_from(limit).unwrap_or(u64::MAX)
    {
        return Err(FetchError::TooLarge);
    }
    let mut total = 0usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| FetchError::Body(error.without_url().to_string()))?;
        total = total.saturating_add(chunk.len());
        if total > limit {
            return Err(FetchError::TooLarge);
        }
        body.push(&chunk).await.map_err(FetchError::BodyWrite)?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) enum FetchError {
    Request(String),
    Status(u16),
    TooLarge,
    Body(String),
    BodyWrite(BodyError),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(formatter, "origin request failed: {error}"),
            // A 3xx the pull did not follow: the client's redirect policy
            // refuses anything outside the source's origin, and an
            // `https`→`http` downgrade when there are credentials.
            Self::Status(status @ 300..=399) => write!(
                formatter,
                "origin answered HTTP {status}, a redirect the pull does not follow"
            ),
            Self::Status(status) => write!(formatter, "origin answered HTTP {status}"),
            Self::TooLarge => write!(formatter, "origin object is over its size bound"),
            Self::Body(error) => write!(formatter, "origin body read failed: {error}"),
            Self::BodyWrite(BodyError::TooLarge) => {
                write!(formatter, "origin object is over the ingest bound")
            }
            Self::BodyWrite(BodyError::BufferFull) => {
                write!(formatter, "HLS ingest buffer is full")
            }
        }
    }
}
