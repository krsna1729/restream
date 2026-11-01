//! Pure verdicts and URL shaping over what moq-relay, its log, Restream's
//! API and Playwright report.
//!
//! Each is a function of captured text or JSON, so every guard is
//! unit-tested on its own; `run.rs` only gathers the inputs.

use super::*;

/// hang's catalog track, published in every broadcast.
pub(crate) const CATALOG_TRACK: &str = "catalog.json";
/// Prefix of the line [`DevRelay`] writes into its log before each start.
pub(crate) const RELAY_START_MARKER: &str = "---- moq-dev-interop: moq-relay start #";
/// Application/stream path of FFmpeg's RTMP read-back listener.
pub(crate) const READBACK_RTMP_PATH: &str = "live/readback";
/// Query key carrying Restream's origin subscribe token.
pub(crate) const ORIGIN_TOKEN_KEY: &str = "token";

/// The authenticated API route minting an origin subscribe grant
/// (`{token, url, broadcast, expiresAt}`, the token good for 60 s of
/// admission) for a pipeline.
pub(crate) fn origin_token_path(pipeline: &str) -> String {
    format!("/api/v1/pipelines/{pipeline}/moq-token")
}

/// A minted origin grant, ready to subscribe with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OriginGrant {
    /// The grant's URL pointed at `127.0.0.1:<port>` (the listener the
    /// harness bound, whatever host Restream advertises), with the token.
    pub(crate) url: String,
    pub(crate) token: String,
    pub(crate) broadcast: String,
}

impl OriginGrant {
    pub(crate) fn parse(grant: &Value, port: u16) -> Result<Self, String> {
        let field = |key: &str| {
            grant
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("origin grant without a {key}: {grant}"))
        };
        let (token, advertised, broadcast) = (field("token")?, field("url")?, field("broadcast")?);
        let mut url = url::Url::parse(advertised)
            .map_err(|error| format!("origin grant url {advertised:?}: {error}"))?;
        if url.scheme() != "https" {
            return Err(format!("origin grant url {advertised:?} is not https://"));
        }
        url.set_host(Some("127.0.0.1"))
            .and_then(|()| {
                url.set_port(Some(port))
                    .map_err(|()| url::ParseError::InvalidPort)
            })
            .map_err(|error| format!("origin grant url {advertised:?}: {error}"))?;
        Ok(Self {
            url: with_token(url.as_str(), Some(token))?,
            token: token.to_string(),
            broadcast: broadcast.to_string(),
        })
    }
}

/// `url` with every `token` query pair removed and, if given, `token` added:
/// the valid, missing and tampered variants of one subscribe URL.
pub(crate) fn with_token(url: &str, token: Option<&str>) -> Result<String, String> {
    let mut url = url::Url::parse(url).map_err(|error| format!("{url:?}: {error}"))?;
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != ORIGIN_TOKEN_KEY)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    if !kept.is_empty() || token.is_some() {
        let mut pairs = url.query_pairs_mut();
        pairs.extend_pairs(&kept);
        if let Some(token) = token {
            pairs.append_pair(ORIGIN_TOKEN_KEY, token);
        }
    }
    Ok(url.to_string())
}

/// `token` with its last character changed: same shape, broken signature.
pub(crate) fn tampered(token: &str) -> String {
    let mut tampered = token.to_string();
    let last = tampered.pop();
    tampered.push(if last == Some('A') { 'B' } else { 'A' });
    tampered
}

/// Every reason a hang catalog does not describe what Restream publishes
/// from the fixture: an H.264 video rendition and an AAC audio rendition,
/// each `description` (avcC / AudioSpecificConfig) hex-encoded, as browsers
/// and moq-cli decode it. `avc1` carries its parameter sets out of band, so
/// it must have a description; `avc3` may not.
pub(crate) fn catalog_failures(catalog: &Value) -> Vec<String> {
    let renditions = |kind: &str| -> Vec<(String, Value)> {
        catalog
            .pointer(&format!("/{kind}/renditions"))
            .and_then(Value::as_object)
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    };
    let codec = |rendition: &Value| {
        rendition
            .get("codec")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut failures = Vec::new();
    let mut description_ok = |track: &str, rendition: &Value, required: bool| match rendition
        .get("description")
        .and_then(Value::as_str)
    {
        Some(hex) if is_hex_bytes(hex) => {}
        Some(other) => failures.push(format!("{track}: description {other:?} is not hex")),
        None if required => failures.push(format!("{track}: avc1 without a description")),
        None => {}
    };

    let video: Vec<_> = renditions("video")
        .into_iter()
        .filter(|(_, rendition)| {
            let codec = codec(rendition);
            codec.starts_with("avc1.") || codec.starts_with("avc3.")
        })
        .collect();
    for (track, rendition) in &video {
        description_ok(track, rendition, codec(rendition).starts_with("avc1."));
    }
    let audio: Vec<_> = renditions("audio")
        .into_iter()
        .filter(|(_, rendition)| codec(rendition).starts_with("mp4a.40."))
        .collect();
    for (track, rendition) in &audio {
        description_ok(track, rendition, false);
    }
    for (track, rendition) in &audio {
        let positive = |key: &str| rendition.get(key).and_then(Value::as_u64).unwrap_or(0) > 0;
        if !positive("sampleRate") || !positive("numberOfChannels") {
            failures.push(format!("{track}: AAC without sampleRate/numberOfChannels"));
        }
    }
    if video.is_empty() {
        failures.push("no H.264 (avc1/avc3) video rendition".to_string());
    }
    if audio.is_empty() {
        failures.push("no AAC (mp4a.40) audio rendition".to_string());
    }
    failures
}

/// The catalog a broadcast must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatalogProfile {
    /// A hang catalog (`catalog.json`).
    Hang,
    /// An MSF catalog whose media tracks are LOC.
    MsfLoc,
    /// An MSF catalog whose media tracks are hang's legacy container, as the
    /// pinned `moq import` publishes it (it has no LOC import).
    MsfLegacy,
}

impl CatalogProfile {
    /// The relay track that carries it.
    pub(crate) fn track(self) -> &'static str {
        match self {
            Self::Hang => CATALOG_TRACK,
            Self::MsfLoc | Self::MsfLegacy => "catalog",
        }
    }
}

/// Parse the real relay response with upstream MSF types. Media tracks must
/// advertise the profile's packaging and usable H.264/AAC decoder configuration.
pub(crate) fn profile_catalog_failures(catalog: &Value, profile: CatalogProfile) -> Vec<String> {
    use base64::Engine as _;

    let packaging = match profile {
        CatalogProfile::Hang => return catalog_failures(catalog),
        CatalogProfile::MsfLoc => moq_msf::Packaging::Loc,
        CatalogProfile::MsfLegacy => moq_msf::Packaging::Legacy,
    };
    let msf = match serde_json::from_value::<moq_msf::Catalog>(catalog.clone()) {
        Ok(msf) => msf,
        Err(error) => return vec![format!("invalid MSF catalog: {error}")],
    };
    let mut failures = Vec::new();
    let mut video = false;
    let mut audio = false;
    for track in &msf.tracks {
        let codec = track.codec.as_deref().unwrap_or_default();
        let is_video = track.role == Some(moq_msf::Role::Video);
        let is_audio = track.role == Some(moq_msf::Role::Audio);
        if !is_video && !is_audio {
            continue;
        }
        if track.packaging != packaging {
            failures.push(format!("{}: expected {packaging:?} packaging", track.name));
        }
        let init_ok = track.init_data.as_deref().is_some_and(|init| {
            base64::engine::general_purpose::STANDARD
                .decode(init)
                .is_ok_and(|bytes| !bytes.is_empty())
        });
        if is_video && (codec.starts_with("avc1.") || codec.starts_with("avc3.")) {
            video = true;
            if codec.starts_with("avc1.") && !init_ok {
                failures.push(format!("{}: avc1 without valid init data", track.name));
            }
        }
        if is_audio && codec.starts_with("mp4a.40.") {
            audio = true;
            if !init_ok
                || track.samplerate.unwrap_or(0) == 0
                || track
                    .channel_config
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
            {
                failures.push(format!(
                    "{}: AAC without init data/sample rate/channels",
                    track.name
                ));
            }
        }
    }
    if !video {
        failures.push("no MSF H.264 video track".to_string());
    }
    if !audio {
        failures.push("no MSF AAC audio track".to_string());
    }
    failures
}

/// The version each outbound dial of relay life `life` connected on, in log
/// order. Only the relay's own dials (its cluster connection) log as a
/// client; sessions it accepts log `negotiated version=`.
pub(crate) fn outbound_dial_versions(log: &str, life: u32) -> Vec<String> {
    let mut current = 0;
    let mut versions = Vec::new();
    for line in log.lines() {
        let line = strip_ansi(line);
        if let Some(start) = line.strip_prefix(RELAY_START_MARKER) {
            current = start.trim().parse().unwrap_or(current + 1);
            continue;
        }
        if current != life {
            continue;
        }
        if let Some(version) = line
            .split_once("moq_tokio::client: connected version=")
            .and_then(|(_, rest)| rest.split_whitespace().next())
        {
            versions.push(version.to_string());
        }
    }
    versions
}

fn is_hex_bytes(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// One session in the moq-relay log, joined from its `session accepted` and
/// `negotiated` lines. `life` counts relay starts, since connection ids
/// restart at zero with every process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RelaySession {
    pub(crate) life: u32,
    pub(crate) conn: u64,
    /// `Publisher` / `Subscriber` from `role=Some(..)`; `None` for `role=None`.
    pub(crate) role: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) transport: Option<String>,
}

impl RelaySession {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "life": self.life,
            "conn": self.conn,
            "role": self.role,
            "version": self.version,
            "transport": self.transport,
        })
    }
}

/// Sessions in a moq-relay 0.17.7 log, in the order they were accepted.
pub(crate) fn relay_sessions(log: &str) -> Vec<RelaySession> {
    let mut sessions: Vec<RelaySession> = Vec::new();
    let mut life = 0;
    for line in log.lines() {
        let line = strip_ansi(line);
        if let Some(start) = line.strip_prefix(RELAY_START_MARKER) {
            life = start.trim().parse().unwrap_or(life + 1);
            continue;
        }
        let Some(conn) = line
            .split_once("conn{id=")
            .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse::<u64>().ok())
        else {
            continue;
        };
        let accepted = line.contains("session accepted");
        let negotiated = line.contains("negotiated version=");
        if !accepted && !negotiated {
            continue;
        }
        let index = match sessions
            .iter()
            .position(|session| session.life == life && session.conn == conn)
        {
            Some(index) => index,
            None => {
                sessions.push(RelaySession {
                    life,
                    conn,
                    ..RelaySession::default()
                });
                sessions.len() - 1
            }
        };
        let session = &mut sessions[index];
        if accepted {
            session.role = log_field(&line, "role").and_then(|role| {
                role.strip_prefix("Some(")
                    .and_then(|inner| inner.strip_suffix(')'))
                    .map(str::to_string)
            });
        }
        if negotiated {
            session.version = log_field(&line, "version").map(str::to_string);
        }
        if let Some(transport) = log_field(&line, "transport") {
            session.transport = Some(transport.to_string());
        }
    }
    sessions
}

/// The value of ` key=value` in a tracing line, up to the next whitespace.
fn log_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!(" {key}=");
    let start = line.find(&needle)? + needle.len();
    line[start..].split_whitespace().next()
}

/// Drop `ESC [ … letter` sequences, in case the relay colours its log.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether relay life `life` saw sessions, all on the pinned version.
/// Called before harness subscribers connect, so those sessions are Restream's.
pub(crate) fn sessions_on_version(
    sessions: &[RelaySession],
    life: u32,
    expected: &str,
) -> (bool, Vec<Value>) {
    let in_life: Vec<&RelaySession> = sessions
        .iter()
        .filter(|session| session.life == life)
        .collect();
    let ok = !in_life.is_empty()
        && in_life
            .iter()
            .all(|session| session.version.as_deref() == Some(expected));
    (
        ok,
        in_life.iter().map(|session| session.to_json()).collect(),
    )
}

/// One Playwright test result for one (browser, endpoint) pair.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrowserOutcome {
    pub(crate) project: String,
    pub(crate) title: String,
    /// Playwright's verdict: `expected`, `unexpected`, `flaky` or `skipped`.
    pub(crate) status: String,
    /// `moq-*` annotations by type (browser version, protocol, playback).
    pub(crate) annotations: serde_json::Map<String, Value>,
    pub(crate) error: Option<String>,
}

/// Every test in a Playwright JSON report (nested suites included).
pub(crate) fn browser_outcomes(report: &Value) -> Vec<BrowserOutcome> {
    fn walk(suite: &Value, out: &mut Vec<BrowserOutcome>) {
        for spec in suite
            .get("specs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let title = spec
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default();
            for test in spec
                .get("tests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let results = test.get("results").and_then(Value::as_array);
                let mut annotations = serde_json::Map::new();
                let all =
                    test.get("annotations")
                        .and_then(Value::as_array)
                        .into_iter()
                        .chain(results.into_iter().flatten().filter_map(|result| {
                            result.get("annotations").and_then(Value::as_array)
                        }))
                        .flatten();
                for annotation in all {
                    if let (Some(kind), Some(text)) = (
                        annotation.get("type").and_then(Value::as_str),
                        annotation.get("description").and_then(Value::as_str),
                    ) && kind.starts_with("moq-")
                    {
                        // Structured annotations (playback) are JSON objects;
                        // the rest (`155.0`, `moq-lite-06/webtransport`) stay text.
                        let value = Some(text)
                            .filter(|text| text.starts_with('{'))
                            .and_then(|text| serde_json::from_str::<Value>(text).ok())
                            .unwrap_or_else(|| json!(text));
                        annotations.insert(kind.to_string(), value);
                    }
                }
                let error = results
                    .and_then(|results| results.last())
                    .and_then(|result| result.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.push(BrowserOutcome {
                    project: test
                        .get("projectName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    title: title.to_string(),
                    status: test
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    annotations,
                    error,
                });
            }
        }
        for child in suite
            .get("suites")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for suite in report
        .get("suites")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk(suite, &mut out);
    }
    out
}

/// How a browser outcome counts. A skipped test is the spec's fallback
/// result (an older Firefox that cannot negotiate moq-lite-06): reported,
/// never a pass and never a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserVerdict {
    Pass,
    Fallback,
    Fail,
}

impl BrowserOutcome {
    pub(crate) fn verdict(&self) -> BrowserVerdict {
        match self.status.as_str() {
            "expected" => BrowserVerdict::Pass,
            "skipped" => BrowserVerdict::Fallback,
            _ => BrowserVerdict::Fail,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "project": self.project,
            "title": self.title,
            "status": self.status,
            "annotations": self.annotations,
            "error": self.error,
        })
    }
}

/// The Playwright projects the browser specs run in.
pub(crate) const BROWSER_PROJECTS: [&str; 2] = ["chromium", "firefox"];

/// One check per (browser, test) pair the run asked for: `plays <target>` as
/// `browser-<project>-<target>` and, for a target that must refuse a
/// token-less page, `refuses <target> without a token` as
/// `browser-<project>-<target>-no-token`. A pair with no result in the
/// report fails: a spec that never ran proves nothing.
pub(crate) fn browser_rows(
    outcomes: &[BrowserOutcome],
    targets: &[(&str, bool)],
) -> Vec<(String, BrowserVerdict, Value)> {
    let mut rows = Vec::new();
    for project in BROWSER_PROJECTS {
        for &(target, refuses) in targets {
            let mut tests = vec![(
                format!("browser-{project}-{target}"),
                format!("plays {target}"),
            )];
            if refuses {
                tests.push((
                    format!("browser-{project}-{target}-no-token"),
                    format!("refuses {target} without a token"),
                ));
            }
            for (id, title) in tests {
                let row = match outcomes
                    .iter()
                    .find(|outcome| outcome.project == project && outcome.title == title)
                {
                    Some(outcome) => (id, outcome.verdict(), outcome.to_json()),
                    None => (
                        id,
                        BrowserVerdict::Fail,
                        json!({"project": project, "title": title, "error": "no result in the Playwright report"}),
                    ),
                };
                rows.push(row);
            }
        }
    }
    rows
}
