//! A small m3u8 parser for HLS push ingest. Pure: bytes in, a playlist or an
//! error out. Malformed input is an error, never a panic.
//!
//! It reads what ordering needs: `#EXTM3U`, `EXT-X-TARGETDURATION`,
//! `EXT-X-MEDIA-SEQUENCE`, `EXTINF`, `EXT-X-DISCONTINUITY`, `EXT-X-MAP`,
//! `EXT-X-ENDLIST` and master playlists (`EXT-X-STREAM-INF` with `BANDWIDTH`
//! and `AUDIO`, and the `EXT-X-MEDIA` renditions those groups name). Every
//! other tag is ignored, except the three that say the media cannot be
//! fetched whole and in the clear — `EXT-X-KEY`/`EXT-X-SESSION-KEY`
//! ([`Encryption`], recorded for the segments actually listed under a key)
//! and `EXT-X-BYTERANGE` ([`MediaPlaylist::byte_range`]) — which are recorded
//! as data. The pull refuses those; push ignores them as
//! it always has. URIs resolve against the playlist's own path; only
//! relative URIs that stay inside the stream key's namespace are accepted.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::time::Duration;

/// Longest `EXT-X-TARGETDURATION` accepted; larger is not a live stream.
const MAX_TARGET_DURATION_SECS: f64 = 3_600.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playlist {
    Media(MediaPlaylist),
    Master(MasterPlaylist),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlaylist {
    pub target_duration: Duration,
    pub media_sequence: u64,
    pub segments: Vec<SegmentEntry>,
    pub ended: bool,
    /// The playlist's last `EXT-X-MAP` init URI, resolved against the
    /// playlist directory like a segment URI: the map its newest segments
    /// use. Each [`SegmentEntry`] records the map in force for itself.
    pub map_uri: Option<String>,
    /// The encryption a listed segment is under: the first `EXT-X-KEY` with a
    /// `METHOD` other than `NONE` that was in force where a segment URI was
    /// reached, or the session key this media playlist names out of place.
    /// Sticky, so a `METHOD=NONE` later in the list does not hide an earlier
    /// encrypted entry. Recorded as data: the pull refuses an encrypted
    /// playlist, push ignores it as it always has.
    pub encryption: Option<Encryption>,
    /// The playlist names `EXT-X-BYTERANGE` on an entry or a `BYTERANGE`
    /// attribute on its `EXT-X-MAP`: an entry is a slice of a resource.
    /// Recorded as data, like [`Self::encryption`].
    pub byte_range: bool,
}

/// An `EXT-X-KEY` (media) or `EXT-X-SESSION-KEY` (master) the playlist
/// names, with a `METHOD` other than `NONE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encryption {
    /// The `METHOD` attribute verbatim (`AES-128`, `SAMPLE-AES`,
    /// `SAMPLE-AES-CTR`). Empty when the tag names none, which the RFC does
    /// not allow: recorded, so the pull still refuses the playlist.
    pub method: String,
    /// The key came from `EXT-X-SESSION-KEY`.
    pub session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentEntry {
    pub sequence: u64,
    /// Object path relative to the stream key, like the PUT that uploads it.
    pub path: String,
    pub duration: Duration,
    /// An `EXT-X-DISCONTINUITY` precedes this segment.
    pub discontinuity: bool,
    /// The `EXT-X-MAP` in force for this segment (the last one before it),
    /// resolved like its URI; `None` for a playlist that names no init.
    pub map_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterPlaylist {
    pub variants: Vec<Variant>,
    /// The master's `EXT-X-MEDIA` renditions of `TYPE=AUDIO`. A variant that
    /// names one of these groups in its `AUDIO` attribute takes its audio
    /// from the group, not from the variant's own segments.
    pub audio: Vec<AudioRendition>,
    /// The `EXT-X-SESSION-KEY` the master names (`METHOD` other than
    /// `NONE`): its media playlists are encrypted. Recorded as data, like
    /// [`MediaPlaylist::encryption`].
    pub session_key: Option<Encryption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub bandwidth: u64,
    pub path: String,
    /// The `AUDIO` group this variant's audio comes from, if it names one.
    /// A variant that names none carries its own audio (RFC 8216 4.3.4.1).
    pub audio_group: Option<String>,
    /// The `CODECS` attribute's entries (`avc1.64001f`, `mp4a.40.2`, …), in
    /// order; empty when the variant declares none.
    pub codecs: Vec<String>,
}

/// One `EXT-X-MEDIA:TYPE=AUDIO` rendition: a named member of the audio group
/// a variant may select with `AUDIO="group"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioRendition {
    pub group_id: String,
    pub name: String,
    /// `DEFAULT=YES`: the member a client plays unless it chooses otherwise.
    pub default: bool,
    /// `AUTOSELECT=YES`: the member a client may play when it has no other
    /// basis for a choice.
    pub autoselect: bool,
    /// The rendition's own media playlist URI, resolved like any other URI.
    /// `None` means the rendition has no URI: its audio is muxed into the
    /// variants of the group, so there is nothing separate to follow.
    pub path: Option<String>,
}

impl MasterPlaylist {
    /// The variant to follow: the highest `BANDWIDTH`, the first on a tie.
    pub fn highest_bandwidth(&self) -> Option<&Variant> {
        self.variants.iter().reduce(|best, next| {
            if next.bandwidth > best.bandwidth {
                next
            } else {
                best
            }
        })
    }

    /// The audio rendition to follow with `variant`, in the order RFC 8216
    /// section 4.3.4.1 gives a client for choosing among the members of one
    /// group: the `DEFAULT=YES` member, else the `AUTOSELECT=YES` member,
    /// else the first listed. `None` when `variant` names no group, and also
    /// when it names one this master does not define — the caller states
    /// which of the two it is, because only the second is a broken master.
    pub fn audio_rendition(&self, variant: &Variant) -> Option<&AudioRendition> {
        let group = variant.audio_group.as_deref()?;
        let members = || self.audio.iter().filter(|audio| audio.group_id == group);
        // `DEFAULT` outranks `AUTOSELECT`; within one kind the first listed
        // wins, so each pass takes the first member that carries its flag.
        members()
            .find(|audio| audio.default)
            .or_else(|| members().find(|audio| audio.autoselect))
            .or_else(|| members().next())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistError {
    NotUtf8,
    MissingHeader,
    MissingTargetDuration,
    MixedMasterAndMedia,
    /// A tag whose value does not parse; `line` is 1-based.
    InvalidTag {
        line: usize,
        tag: &'static str,
    },
    SegmentWithoutDuration {
        line: usize,
    },
    VariantWithoutUri,
    InvalidUri {
        line: usize,
    },
    SequenceOverflow,
}

impl fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUtf8 => write!(f, "playlist is not UTF-8"),
            Self::MissingHeader => write!(f, "playlist does not start with #EXTM3U"),
            Self::MissingTargetDuration => write!(f, "media playlist lacks EXT-X-TARGETDURATION"),
            Self::MixedMasterAndMedia => write!(f, "playlist mixes master and media tags"),
            Self::InvalidTag { line, tag } => write!(f, "line {line}: invalid {tag}"),
            Self::SegmentWithoutDuration { line } => {
                write!(f, "line {line}: segment URI without EXTINF")
            }
            Self::VariantWithoutUri => write!(f, "EXT-X-STREAM-INF without a URI"),
            Self::InvalidUri { line } => write!(f, "line {line}: unsupported URI"),
            Self::SequenceOverflow => write!(f, "media sequence overflows"),
        }
    }
}

impl std::error::Error for PlaylistError {}

/// Parse the playlist uploaded at `playlist_path` (already normalized).
pub fn parse(bytes: &[u8], playlist_path: &str) -> Result<Playlist, PlaylistError> {
    parse_with(bytes, |uri| resolve_uri(playlist_path, uri))
}

/// Parse a playlist whose URIs a caller resolves (push uploads against their
/// namespace, pull ingest against the remote playlist URL). The resolver gets
/// each raw URI; `None` rejects it.
pub fn parse_with(
    bytes: &[u8],
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> Result<Playlist, PlaylistError> {
    let text = std::str::from_utf8(bytes).map_err(|_| PlaylistError::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index.saturating_add(1), line.trim()))
        .filter(|(_, line)| !line.is_empty());
    match lines.next() {
        Some((_, "#EXTM3U")) => {}
        _ => return Err(PlaylistError::MissingHeader),
    }

    let mut target_duration = None;
    let mut media_sequence = 0u64;
    let mut ended = false;
    let mut map_uri = None;
    let mut byte_range = false;
    // The key in force, and the encryption a *listed segment* is under: the
    // latter is sticky and records the first non-`NONE` method a segment was
    // listed with, which is what makes an encrypted entry anywhere in the
    // list visible however the list ends.
    let mut key_in_force: Option<Encryption> = None;
    let mut media_encryption: Option<Encryption> = None;
    let mut session_encryption = None;
    let mut pending_duration = None;
    let mut pending_discontinuity = false;
    let mut pending_bandwidth = None;
    let mut pending_audio_group = None;
    let mut pending_codecs: Vec<String> = Vec::new();
    let mut segment_uris: Vec<(String, Duration, bool, Option<String>)> = Vec::new();
    let mut variants = Vec::new();
    let mut audio = Vec::new();

    for (line_number, line) in lines {
        let invalid = |tag| PlaylistError::InvalidTag {
            line: line_number,
            tag,
        };
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = Some(
                parse_seconds(value)
                    .filter(|duration| duration.as_secs_f64() <= MAX_TARGET_DURATION_SECS)
                    .filter(|duration| !duration.is_zero())
                    .ok_or_else(|| invalid("EXT-X-TARGETDURATION"))?,
            );
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = value
                .trim()
                .parse()
                .map_err(|_| invalid("EXT-X-MEDIA-SEQUENCE"))?;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let duration = value.split(',').next().unwrap_or_default();
            pending_duration = Some(parse_seconds(duration).ok_or_else(|| invalid("EXTINF"))?);
        } else if line == "#EXT-X-DISCONTINUITY" {
            pending_discontinuity = true;
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if line.starts_with("#EXT-X-BYTERANGE:") {
            // Recorded whether or not the tag carries a range: an entry is a
            // slice either way. Neither tag becomes a parse error here, both
            // were ignored before, and push must go on reading what it
            // always read.
            byte_range = true;
        } else if let Some(attributes) = key_attributes(line) {
            // Both tags carry the same attributes; only which one is
            // remembered differs. `METHOD=NONE` clears the key, as in the
            // RFC's own example, and a tag that names no `METHOD` — which the
            // RFC requires — is recorded with an empty method rather than
            // refused.
            let session = line.starts_with("#EXT-X-SESSION-KEY:");
            let method = attribute(attributes, "METHOD")
                .unwrap_or_default()
                .to_string();
            let encryption =
                (!method.eq_ignore_ascii_case("NONE")).then_some(Encryption { method, session });
            if session {
                session_encryption = encryption;
            } else {
                key_in_force = encryption;
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = attribute(value, "URI").ok_or_else(|| invalid("EXT-X-MAP"))?;
            // A map with its own `BYTERANGE` is a slice too: the pull would
            // fetch the whole resource as the init.
            byte_range |= attribute(value, "BYTERANGE").is_some_and(|range| !range.is_empty());
            map_uri = Some(resolve(uri).ok_or(PlaylistError::InvalidUri { line: line_number })?);
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-MEDIA:") {
            // Only audio groups matter here: `SUBTITLES` and
            // `CLOSED-CAPTIONS` renditions carry no media this ingest
            // publishes, so they are ignored like any other tag.
            match attribute(attributes, "TYPE") {
                Some(kind) if kind.eq_ignore_ascii_case("AUDIO") => {}
                _ => continue,
            }
            let group_id = attribute(attributes, "GROUP-ID")
                .filter(|group| !group.is_empty())
                .ok_or_else(|| invalid("EXT-X-MEDIA"))?;
            // A missing URI means the group's audio is muxed into the
            // variants that name it, so nothing separate can be followed.
            let path = match attribute(attributes, "URI").filter(|uri| !uri.is_empty()) {
                Some(uri) => {
                    Some(resolve(uri).ok_or(PlaylistError::InvalidUri { line: line_number })?)
                }
                None => None,
            };
            audio.push(AudioRendition {
                group_id: group_id.to_string(),
                name: attribute(attributes, "NAME")
                    .unwrap_or_default()
                    .to_string(),
                default: attribute(attributes, "DEFAULT")
                    .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
                autoselect: attribute(attributes, "AUTOSELECT")
                    .is_some_and(|value| value.eq_ignore_ascii_case("YES")),
                path,
            });
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bandwidth = attribute(attributes, "BANDWIDTH")
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid("EXT-X-STREAM-INF"))?;
            pending_bandwidth = Some(bandwidth);
            pending_audio_group = attribute(attributes, "AUDIO")
                .filter(|group| !group.is_empty())
                .map(str::to_string);
            pending_codecs = attribute(attributes, "CODECS")
                .map(|codecs| {
                    codecs
                        .split(',')
                        .map(str::trim)
                        .filter(|codec| !codec.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
        } else if line.starts_with('#') {
            // Comments and tags this ingest does not need.
        } else {
            let path = resolve(line).ok_or(PlaylistError::InvalidUri { line: line_number })?;
            if let Some(bandwidth) = pending_bandwidth.take() {
                variants.push(Variant {
                    bandwidth,
                    path,
                    audio_group: pending_audio_group.take(),
                    codecs: std::mem::take(&mut pending_codecs),
                });
            } else {
                let duration = pending_duration
                    .take()
                    .ok_or(PlaylistError::SegmentWithoutDuration { line: line_number })?;
                // This entry is under the key in force, whatever the list
                // does after it.
                if media_encryption.is_none() {
                    media_encryption.clone_from(&key_in_force);
                }
                segment_uris.push((
                    path,
                    duration,
                    std::mem::take(&mut pending_discontinuity),
                    map_uri.clone(),
                ));
            }
        }
    }

    if pending_bandwidth.is_some() {
        return Err(PlaylistError::VariantWithoutUri);
    }
    if !variants.is_empty() {
        if !segment_uris.is_empty() || target_duration.is_some() {
            return Err(PlaylistError::MixedMasterAndMedia);
        }
        return Ok(Playlist::Master(MasterPlaylist {
            variants,
            audio,
            session_key: session_encryption,
        }));
    }

    let target_duration = target_duration.ok_or(PlaylistError::MissingTargetDuration)?;
    let mut segments = Vec::with_capacity(segment_uris.len());
    for (offset, (path, duration, discontinuity, map_uri)) in segment_uris.into_iter().enumerate() {
        let sequence = u64::try_from(offset)
            .ok()
            .and_then(|offset| media_sequence.checked_add(offset))
            .ok_or(PlaylistError::SequenceOverflow)?;
        segments.push(SegmentEntry {
            sequence,
            path,
            duration,
            discontinuity,
            map_uri,
        });
    }
    Ok(Playlist::Media(MediaPlaylist {
        target_duration,
        media_sequence,
        segments,
        ended,
        map_uri,
        // A media playlist that names a session key is out of place; it is
        // still recorded, so the pull refuses the playlist rather than
        // demuxing encrypted frames as media.
        encryption: media_encryption.or(session_encryption),
        byte_range,
    }))
}

/// The attribute list of an `EXT-X-KEY` or `EXT-X-SESSION-KEY` line, if it is
/// one.
fn key_attributes(line: &str) -> Option<&str> {
    line.strip_prefix("#EXT-X-KEY:")
        .or_else(|| line.strip_prefix("#EXT-X-SESSION-KEY:"))
}

fn parse_seconds(value: &str) -> Option<Duration> {
    let seconds: f64 = value.trim().parse().ok()?;
    Duration::try_from_secs_f64(seconds).ok()
}

/// One value of an attribute list (`KEY=value,KEY="quoted, value"`).
fn attribute<'a>(list: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = list;
    while !rest.is_empty() {
        let (name, after_name) = rest.split_once('=')?;
        let (value, after_value) = match after_name.strip_prefix('"') {
            Some(quoted) => {
                let (value, tail) = quoted.split_once('"')?;
                (value, tail.strip_prefix(',').unwrap_or(tail))
            }
            None => after_name.split_once(',').unwrap_or((after_name, "")),
        };
        if name.trim() == key {
            return Some(value.trim());
        }
        rest = after_value;
    }
    None
}

/// Resolve `uri` against the directory of `playlist_path`. Rejects absolute
/// URIs and paths that leave the stream key's namespace.
fn resolve_uri(playlist_path: &str, uri: &str) -> Option<String> {
    let uri = uri.split(['?', '#']).next().unwrap_or_default();
    if uri.is_empty() || uri.contains("://") || uri.starts_with('/') {
        return None;
    }
    let directory = playlist_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    normalize_path(&format!("{directory}/{uri}"))
}

/// Normalize an object path relative to the stream key: no empty result, no
/// escape above the root, no `\` or control characters.
pub fn normalize_path(path: &str) -> Option<String> {
    if path.chars().any(|c| c == '\\' || c.is_control()) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
#[path = "playlist_tests.rs"]
mod tests;
