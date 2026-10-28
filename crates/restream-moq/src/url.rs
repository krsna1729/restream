//! `moqt://` output URLs:
//! `moqt://host[:port][/path][?broadcast=<name>&ca=<file>&insecure=1&version=<alpn>]`.

use percent_encoding::percent_decode_str;

pub const DEFAULT_MOQ_PORT: u16 = 443;

const BROADCAST_KEY: &str = "broadcast";
const CA_KEY: &str = "ca";
const INSECURE_KEY: &str = "insecure";
const VERSION_KEY: &str = "version";

pub const MOQ_ALPNS: &[&str] = moq_net::ALPNS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub broadcast: Option<String>,
    pub ca_file: Option<String>,
    pub insecure: bool,
    pub version: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoqUrlError {
    Malformed,
    NotMoqt,
    MissingHost,
    EmptyBroadcast,
    Credentials,
    InvalidInsecure,
    EmptyCa,
    UnknownVersion,
}

impl MoqUrlError {
    pub const fn message(self) -> &'static str {
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

pub fn parse_moq_url(url: &str) -> Result<MoqUrl, MoqUrlError> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url).map_err(|_| MoqUrlError::Malformed)?;
    if !parsed.scheme().eq_ignore_ascii_case("moqt") {
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

fn decode_component(raw: &str) -> String {
    let spaced = raw.replace('+', " ");
    percent_decode_str(&spaced).decode_utf8_lossy().into_owned()
}
