//! HTTP Digest authentication (RFC 7616) for a pull origin that answers `401`
//! with a `WWW-Authenticate: Digest` challenge, as FFmpeg's HTTP client
//! does: the operator's username and password answer whichever scheme the
//! origin asks for. Supported: `MD5`, `MD5-sess`, `SHA-256` and
//! `SHA-256-sess`, with `qop=auth` or no `qop` (RFC 2069). `auth-int` and
//! `userhash` are not: an origin that offers only those is refused with that
//! reason.

use std::fmt::Write as _;

use md5::Md5;
use sha2::{Digest as _, Sha256};

/// One hash function a challenge can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Algorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
}

impl Algorithm {
    fn parse(name: &str) -> Option<Self> {
        [
            ("MD5", Self::Md5),
            ("MD5-sess", Self::Md5Sess),
            ("SHA-256", Self::Sha256),
            ("SHA-256-sess", Self::Sha256Sess),
        ]
        .into_iter()
        .find_map(|(token, algorithm)| name.eq_ignore_ascii_case(token).then_some(algorithm))
    }

    fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Md5Sess => "MD5-sess",
            Self::Sha256 => "SHA-256",
            Self::Sha256Sess => "SHA-256-sess",
        }
    }

    fn is_session(self) -> bool {
        matches!(self, Self::Md5Sess | Self::Sha256Sess)
    }

    /// Lower-case hex of the hash of `parts` joined by `:`.
    fn hash(self, parts: &[&str]) -> String {
        let joined = parts.join(":");
        match self {
            Self::Md5 | Self::Md5Sess => hex(&Md5::digest(joined.as_bytes())),
            Self::Sha256 | Self::Sha256Sess => hex(&Sha256::digest(joined.as_bytes())),
        }
    }

    /// RFC 7616 §3.7 prefers SHA-256 when an origin offers several.
    fn rank(self) -> u8 {
        match self {
            Self::Sha256 | Self::Sha256Sess => 1,
            Self::Md5 | Self::Md5Sess => 0,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A Digest challenge the pull can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Challenge {
    pub(super) realm: String,
    pub(super) nonce: String,
    pub(super) opaque: Option<String>,
    pub(super) algorithm: Algorithm,
    /// `qop=auth` was offered. Without it the RFC 2069 form is used.
    pub(super) qop_auth: bool,
    pub(super) stale: bool,
}

/// Why no Digest challenge in a `401` can be answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// No `WWW-Authenticate` value names the Digest scheme.
    NotDigest,
    /// A Digest challenge exists, but the pull cannot answer it: the reason.
    Unsupported(String),
}

/// The best answerable Digest challenge among a response's
/// `WWW-Authenticate` values: SHA-256 before MD5.
pub(super) fn select<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<Challenge, Refusal> {
    let mut best: Option<Challenge> = None;
    let mut refused: Option<String> = None;
    for value in values {
        let Some(params) = digest_params(value) else {
            continue;
        };
        match challenge(&params) {
            Ok(candidate) => {
                if best
                    .as_ref()
                    .is_none_or(|current| candidate.algorithm.rank() > current.algorithm.rank())
                {
                    best = Some(candidate);
                }
            }
            Err(reason) => refused = Some(reason),
        }
    }
    match (best, refused) {
        (Some(challenge), _) => Ok(challenge),
        (None, Some(reason)) => Err(Refusal::Unsupported(reason)),
        (None, None) => Err(Refusal::NotDigest),
    }
}

fn challenge(params: &[(String, String)]) -> Result<Challenge, String> {
    let get = |key: &str| {
        params
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    };
    let algorithm_name = get("algorithm").unwrap_or("MD5");
    let algorithm = Algorithm::parse(algorithm_name).ok_or_else(|| {
        format!("the origin's Digest algorithm {algorithm_name} is not supported")
    })?;
    if get("userhash").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
        return Err("the origin requires a Digest userhash, which is not supported".to_string());
    }
    let qop_auth = match get("qop") {
        None => false,
        Some(offered) => {
            if !offered
                .split(',')
                .any(|qop| qop.trim().eq_ignore_ascii_case("auth"))
            {
                return Err(format!(
                    "the origin's Digest qop {offered} is not supported"
                ));
            }
            true
        }
    };
    let nonce = get("nonce").ok_or("the origin's Digest challenge has no nonce")?;
    Ok(Challenge {
        realm: get("realm").unwrap_or_default().to_string(),
        nonce: nonce.to_string(),
        opaque: get("opaque").map(str::to_string),
        algorithm,
        qop_auth,
        stale: get("stale").is_some_and(|value| value.eq_ignore_ascii_case("true")),
    })
}

/// The `key=value` parameters of a `Digest` challenge, or `None` when the
/// value names another scheme. Values may be quoted, and a quoted value may
/// hold commas and `\`-escaped characters.
fn digest_params(value: &str) -> Option<Vec<(String, String)>> {
    let value = value.trim_start();
    let (scheme, rest) = value.split_at(value.find(char::is_whitespace).unwrap_or(value.len()));
    if !scheme.eq_ignore_ascii_case("Digest") {
        return None;
    }
    let mut params = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == ',').is_some() {}
        let name: String =
            std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if name.trim().is_empty() {
            break;
        }
        if chars.next_if_eq(&'=').is_none() {
            params.push((name.trim().to_string(), String::new()));
            continue;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut value = String::new();
        if chars.next_if_eq(&'"').is_some() {
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            value.extend(std::iter::from_fn(|| chars.next_if(|c| *c != ',')));
            value.truncate(value.trim_end().len());
        }
        params.push((name.trim().to_string(), value));
    }
    Some(params)
}

/// The `Authorization` value answering `challenge` for one request. `nc` is
/// the count of requests made with this nonce, starting at 1; `cnonce` is a
/// fresh client nonce.
pub(super) fn authorization(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    nc: u32,
    cnonce: &str,
) -> String {
    let algorithm = challenge.algorithm;
    let mut ha1 = algorithm.hash(&[username, &challenge.realm, password]);
    if algorithm.is_session() {
        ha1 = algorithm.hash(&[&ha1, &challenge.nonce, cnonce]);
    }
    let ha2 = algorithm.hash(&[method, uri]);
    let nc = format!("{nc:08x}");
    let response = if challenge.qop_auth {
        algorithm.hash(&[&ha1, &challenge.nonce, &nc, cnonce, "auth", &ha2])
    } else {
        algorithm.hash(&[&ha1, &challenge.nonce, &ha2])
    };
    let mut header = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm={}, response=\"{response}\"",
        quote(username),
        quote(&challenge.realm),
        quote(&challenge.nonce),
        quote(uri),
        algorithm.name(),
    );
    if challenge.qop_auth {
        let _ = write!(header, ", qop=auth, nc={nc}, cnonce=\"{}\"", quote(cnonce));
    }
    if let Some(opaque) = &challenge.opaque {
        let _ = write!(header, ", opaque=\"{}\"", quote(opaque));
    }
    header
}

fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
