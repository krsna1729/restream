//! `moqt://` output URLs:
//! `moqt://host[:port][/path][?broadcast=<name>&ca=<file>&insecure=1&version=<alpn>]`.
//!
//! The relay address and the session path (the relay's scope, e.g. `/anon`)
//! come from the URL. Four query keys belong to Restream and never reach
//! the relay: `broadcast` (the broadcast name announced under that scope;
//! the pipeline id when absent), `ca` (a PEM file of extra trusted CA
//! certificates, for a relay with a private CA), `insecure` (skip
//! certificate verification) and `version` (offer only this MoQ ALPN, e.g.
//! `moqt-19` or `moq-lite-06`; by default every version moq-net speaks is
//! offered and the relay picks). Every other query pair stays in the path
//! sent to the relay, in its original encoding (e.g. `?jwt=`).

use percent_encoding::percent_decode_str;

use crate::domain::output_spec::OutputUrlScheme;
use crate::media::moq::transport::MOQ_ALPNS;

/// Port used when the URL names none: the MoQ client convention (MediaMTX
/// appends `:443` too), not the relay listener's own default.
pub(crate) const DEFAULT_MOQ_PORT: u16 = 443;

/// Query key naming the broadcast.
const BROADCAST_KEY: &str = "broadcast";
/// Query key naming a PEM file of extra trusted CA certificates.
const CA_KEY: &str = "ca";
/// Query key disabling relay certificate verification.
const INSECURE_KEY: &str = "insecure";
/// Query key restricting the offered MoQ versions to one ALPN.
const VERSION_KEY: &str = "version";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoqUrl {
    /// Host to resolve and to name in TLS SNI (IPv6 without brackets).
    pub(crate) host: String,
    pub(crate) port: u16,
    /// The session path sent to the relay: URL path (without a trailing
    /// `/`; empty for none) plus every query pair that is not Restream's
    /// own, e.g. `/anon?jwt=x`.
    pub(crate) path: String,
    /// The broadcast name, percent-decoded; `None` means the pipeline id.
    pub(crate) broadcast: Option<String>,
    /// Extra trusted CA certificates (PEM file path), percent-decoded.
    pub(crate) ca_file: Option<String>,
    pub(crate) insecure: bool,
    /// The only MoQ ALPN to offer; `None` offers every one.
    pub(crate) version: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoqUrlError {
    Malformed,
    NotMoqt,
    MissingHost,
    /// `broadcast=` names no broadcast.
    EmptyBroadcast,
    /// Credentials in the URL are not carried to the relay.
    Credentials,
    InvalidInsecure,
    EmptyCa,
    /// `version=` names no MoQ version moq-net speaks.
    UnknownVersion,
}

impl MoqUrlError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Malformed => "MoQ output URL must be a valid absolute URL",
            Self::NotMoqt => "MoQ output URL scheme must be moqt://",
            Self::MissingHost => "MoQ output URL must name a relay host",
            Self::EmptyBroadcast => "MoQ output URL `broadcast` must name a broadcast",
            Self::Credentials => "MoQ output URL must not carry a user name or password",
            Self::InvalidInsecure => "MoQ output URL `insecure` must be 1, 0, true or false",
            Self::EmptyCa => "MoQ output URL `ca` must name a PEM file",
            Self::UnknownVersion => {
                "MoQ output URL `version` must be a MoQ ALPN such as moqt-19 or moq-lite-06"
            }
        }
    }
}

pub(crate) fn parse_moq_url(url: &str) -> Result<MoqUrl, MoqUrlError> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| MoqUrlError::Malformed)?;
    if OutputUrlScheme::from_url(url) != OutputUrlScheme::Moqt {
        return Err(MoqUrlError::NotMoqt);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(MoqUrlError::Credentials);
    }
    let host = parsed
        .host_str()
        .map(|host| host.trim_matches(['[', ']']))
        .filter(|host| !host.is_empty())
        .ok_or(MoqUrlError::MissingHost)?
        .to_string();
    let port = parsed.port().unwrap_or(DEFAULT_MOQ_PORT);
    let url_path = parsed.path().trim_end_matches('/');

    let mut broadcast = None;
    let mut ca_file = None;
    let mut insecure = false;
    let mut version = None;
    let mut relay_query = String::new();
    for pair in parsed.query().unwrap_or_default().split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_component(raw_key);
        if key == CA_KEY {
            let value = decode_component(raw_value);
            if value.is_empty() {
                return Err(MoqUrlError::EmptyCa);
            }
            ca_file = Some(value);
        } else if key == BROADCAST_KEY {
            let value = decode_component(raw_value);
            if value.trim_matches('/').is_empty() {
                return Err(MoqUrlError::EmptyBroadcast);
            }
            broadcast = Some(value);
        } else if key == INSECURE_KEY {
            insecure = match decode_component(raw_value).as_str() {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => return Err(MoqUrlError::InvalidInsecure),
            };
        } else if key == VERSION_KEY {
            let value = decode_component(raw_value);
            version = Some(
                MOQ_ALPNS
                    .iter()
                    .copied()
                    .find(|alpn| *alpn == value)
                    .ok_or(MoqUrlError::UnknownVersion)?,
            );
        } else {
            if !relay_query.is_empty() {
                relay_query.push('&');
            }
            relay_query.push_str(pair);
        }
    }

    let path = if relay_query.is_empty() {
        url_path.to_string()
    } else {
        format!("{url_path}?{relay_query}")
    };
    Ok(MoqUrl {
        host,
        port,
        path,
        broadcast,
        ca_file,
        insecure,
        version,
    })
}

/// A query key or value, decoded the way HTML forms encode them (`+` is a
/// space).
fn decode_component(raw: &str) -> String {
    let spaced = raw.replace('+', " ");
    percent_decode_str(&spaced).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
