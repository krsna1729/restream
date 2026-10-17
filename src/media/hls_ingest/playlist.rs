//! A small m3u8 parser for HLS push ingest. Pure: bytes in, a playlist or an
//! error out. Malformed input is an error, never a panic.
//!
//! It reads what ordering needs: `#EXTM3U`, `EXT-X-TARGETDURATION`,
//! `EXT-X-MEDIA-SEQUENCE`, `EXTINF`, `EXT-X-DISCONTINUITY`, `EXT-X-MAP`,
//! `EXT-X-ENDLIST` and master playlists (`EXT-X-STREAM-INF` with `BANDWIDTH`
//! and `AUDIO`, and the `EXT-X-MEDIA` renditions those groups name). Every
//! other tag is ignored. URIs resolve against the playlist's own path; only
//! relative URIs that stay inside the stream key's namespace are accepted.

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub bandwidth: u64,
    pub path: String,
    /// The `AUDIO` group this variant's audio comes from, if it names one.
    /// A variant that names none carries its own audio (RFC 8216 4.3.4.1).
    pub audio_group: Option<String>,
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

    /// The variant to follow: the lowest `BANDWIDTH`, the first on a tie.
    pub fn lowest_bandwidth(&self) -> Option<&Variant> {
        self.variants.iter().reduce(|best, next| {
            if next.bandwidth < best.bandwidth {
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
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty());
    match lines.next() {
        Some((_, "#EXTM3U")) => {}
        _ => return Err(PlaylistError::MissingHeader),
    }

    let mut target_duration = None;
    let mut media_sequence = 0u64;
    let mut ended = false;
    let mut map_uri = None;
    let mut pending_duration = None;
    let mut pending_discontinuity = false;
    let mut pending_bandwidth = None;
    let mut pending_audio_group = None;
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
        } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = attribute(value, "URI").ok_or_else(|| invalid("EXT-X-MAP"))?;
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
        } else if line.starts_with('#') {
            // Comments and tags this ingest does not need.
        } else {
            let path = resolve(line).ok_or(PlaylistError::InvalidUri { line: line_number })?;
            if let Some(bandwidth) = pending_bandwidth.take() {
                variants.push(Variant {
                    bandwidth,
                    path,
                    audio_group: pending_audio_group.take(),
                });
            } else {
                let duration = pending_duration
                    .take()
                    .ok_or(PlaylistError::SegmentWithoutDuration { line: line_number })?;
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
        return Ok(Playlist::Master(MasterPlaylist { variants, audio }));
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
    }))
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
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn media(text: &str) -> MediaPlaylist {
        match parse(text.as_bytes(), "live/index.m3u8") {
            Ok(Playlist::Media(playlist)) => playlist,
            other => panic!("expected media playlist, got {other:?}"),
        }
    }

    #[test]
    fn media_playlist_lists_segments_by_sequence_resolved_to_the_playlist_directory() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n\
             #EXTINF:2.002,\nindex7.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:1.5,title\n\
             ../other/index8.ts?token=1\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(playlist.target_duration, Duration::from_secs(2));
        assert_eq!(playlist.media_sequence, 7);
        assert!(playlist.ended);
        assert_eq!(
            playlist.segments,
            vec![
                SegmentEntry {
                    sequence: 7,
                    path: "live/index7.ts".into(),
                    duration: Duration::from_secs_f64(2.002),
                    discontinuity: false,
                    map_uri: None,
                },
                SegmentEntry {
                    sequence: 8,
                    path: "other/index8.ts".into(),
                    duration: Duration::from_secs_f64(1.5),
                    discontinuity: true,
                    map_uri: None,
                },
            ]
        );
    }

    #[test]
    fn master_playlist_variants_carry_bandwidth_and_resolved_paths() {
        let text = "#EXTM3U\r\n#EXT-X-STREAM-INF:CODECS=\"avc1.64001f,mp4a.40.2\",BANDWIDTH=800000\r\n\
                    low/index.m3u8\r\n#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\r\n\
                    high/index.m3u8\r\n";
        let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
            panic!("expected master playlist");
        };
        assert_eq!(master.variants.len(), 2);
        assert_eq!(
            master.highest_bandwidth(),
            Some(&Variant {
                bandwidth: 2_500_000,
                path: "high/index.m3u8".into(),
                audio_group: None,
            })
        );
        assert_eq!(master.variants[0].path, "low/index.m3u8");
        assert_eq!(
            master.lowest_bandwidth(),
            Some(&Variant {
                bandwidth: 800_000,
                path: "low/index.m3u8".into(),
                audio_group: None,
            })
        );
    }

    /// A master whose audio is a group of `EXT-X-MEDIA` renditions, as
    /// packaged live HLS writes it: one rendition per language, each with its
    /// own playlist URI, and the variant naming the group.
    #[test]
    fn master_playlist_reads_the_audio_group_and_the_variant_that_names_it() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1 (en)\",LANGUAGE=\"en\",\
            DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio/en/index.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 2 (de)\",LANGUAGE=\"de\",\
            DEFAULT=NO,AUTOSELECT=YES,URI=\"audio/de/index.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2500000,AUDIO=\"audio\"\n\
            video/index.m3u8\n";
        let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
            panic!("expected master playlist");
        };
        assert_eq!(
            master.audio,
            vec![
                AudioRendition {
                    group_id: "audio".into(),
                    name: "Track 1 (en)".into(),
                    default: true,
                    autoselect: true,
                    path: Some("audio/en/index.m3u8".into()),
                },
                AudioRendition {
                    group_id: "audio".into(),
                    name: "Track 2 (de)".into(),
                    default: false,
                    autoselect: true,
                    path: Some("audio/de/index.m3u8".into()),
                },
            ]
        );
        let variant = master.highest_bandwidth().expect("one variant");
        assert_eq!(variant.audio_group.as_deref(), Some("audio"));
        assert_eq!(
            master
                .audio_rendition(variant)
                .map(|audio| audio.name.as_str()),
            Some("Track 1 (en)"),
            "DEFAULT=YES wins"
        );
    }

    #[test]
    fn audio_rendition_selection_prefers_default_then_autoselect_then_the_first() {
        let rendition = |name: &str, default: bool, autoselect: bool| AudioRendition {
            group_id: "audio".into(),
            name: name.into(),
            default,
            autoselect,
            path: Some(format!("audio/{name}.m3u8")),
        };
        let variant = Variant {
            bandwidth: 1,
            path: "video/index.m3u8".into(),
            audio_group: Some("audio".into()),
        };
        let chosen = |audio: Vec<AudioRendition>| {
            let master = MasterPlaylist {
                variants: vec![variant.clone()],
                audio,
            };
            master
                .audio_rendition(&variant)
                .map(|audio| audio.name.clone())
        };

        assert_eq!(
            chosen(vec![
                rendition("first", false, false),
                rendition("default", true, false),
                rendition("autoselect", false, true),
            ])
            .as_deref(),
            Some("default"),
            "DEFAULT=YES outranks AUTOSELECT=YES and the first listed"
        );
        assert_eq!(
            chosen(vec![
                rendition("first", false, false),
                rendition("autoselect", false, true),
            ])
            .as_deref(),
            Some("autoselect"),
            "AUTOSELECT=YES outranks the first listed"
        );
        assert_eq!(
            chosen(vec![
                rendition("first", false, false),
                rendition("second", false, false),
            ])
            .as_deref(),
            Some("first"),
            "the first listed wins when neither flag is set"
        );
        assert_eq!(
            chosen(vec![
                rendition("first-default", true, false),
                rendition("second-default", true, false),
            ])
            .as_deref(),
            Some("first-default"),
            "the first of a tie wins"
        );
        assert_eq!(
            chosen(vec![]),
            None,
            "a group with no member selects nothing"
        );
        // A variant that names no group has no rendition to select.
        let no_group = MasterPlaylist {
            variants: vec![Variant {
                bandwidth: 1,
                path: "video/index.m3u8".into(),
                audio_group: None,
            }],
            audio: vec![rendition("audio", true, true)],
        };
        assert_eq!(no_group.audio_rendition(&no_group.variants[0]), None);
    }

    #[test]
    fn audio_renditions_without_a_uri_are_muxed_into_the_variant() {
        // `URI` is optional: without it the group's audio is muxed into the
        // variants that name the group, so there is no playlist to follow.
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Track 1\",DEFAULT=YES\n\
            #EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"audio\"\n\
            video/index.m3u8\n";
        let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
            panic!("expected master playlist");
        };
        assert_eq!(master.audio.len(), 1);
        assert_eq!(master.audio[0].path, None);
        assert_eq!(
            master
                .audio_rendition(master.highest_bandwidth().expect("one variant"))
                .and_then(|audio| audio.path.as_deref()),
            None
        );
    }

    #[test]
    fn non_audio_media_renditions_are_ignored() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",\
            URI=\"subs/en/index.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"CC1\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=100\n\
            video/index.m3u8\n";
        let Ok(Playlist::Master(master)) = parse(text.as_bytes(), "master.m3u8") else {
            panic!("expected master playlist");
        };
        assert!(master.audio.is_empty(), "{:?}", master.audio);
    }

    #[test]
    fn a_malformed_audio_rendition_is_an_error() {
        // `GROUP-ID` is required on `EXT-X-MEDIA`.
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,NAME=\"Track 1\",URI=\"a.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1\nvideo/index.m3u8\n",
                "master.m3u8"
            ),
            Err(PlaylistError::InvalidTag {
                line: 2,
                tag: "EXT-X-MEDIA",
            })
        );
        // A rendition URI resolves like any other: it may not leave the
        // namespace.
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",URI=\"../../a.m3u8\"\n\
                  #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"audio\"\nvideo/index.m3u8\n",
                "master.m3u8"
            ),
            Err(PlaylistError::InvalidUri { line: 2 })
        );
    }

    #[test]
    fn unknown_tags_and_comments_are_ignored() {
        let playlist = media(
            "#EXTM3U\n# a comment\n#EXT-X-TARGETDURATION:4\n#EXT-X-PROGRAM-DATE-TIME:2026-01-01T00:00:00Z\n\
             #EXT-X-UNKNOWN:x\n#EXTINF:4,\nseg.ts\n",
        );
        assert_eq!(playlist.segments.len(), 1);
        assert!(!playlist.ended);
    }

    #[test]
    fn map_tag_resolves_the_init_uri_against_the_playlist_directory() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n\
             #EXTINF:2,\nindex7.ts\n",
        );
        assert_eq!(playlist.map_uri.as_deref(), Some("live/init.mp4"));

        let playlist = media("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nindex7.ts\n");
        assert_eq!(playlist.map_uri, None);
    }

    #[test]
    fn later_map_tag_overwrites_an_earlier_one() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"a/init.mp4\"\n\
             #EXTINF:2,\nindex7.ts\n#EXT-X-MAP:URI=\"b/init2.mp4\"\n#EXTINF:2,\nindex8.ts\n",
        );
        assert_eq!(playlist.map_uri.as_deref(), Some("live/b/init2.mp4"));
        // A mid-listing change is recorded per segment: index7.ts belongs to
        // the map that was in force when it was listed.
        let maps: Vec<Option<&str>> = playlist
            .segments
            .iter()
            .map(|segment| segment.map_uri.as_deref())
            .collect();
        assert_eq!(
            maps,
            vec![Some("live/a/init.mp4"), Some("live/b/init2.mp4")]
        );
    }

    #[test]
    fn malformed_map_tags_are_errors() {
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:BYTERANGE=\"100@200\"\n\
                  #EXTINF:2,\nindex7.ts\n",
                "live/index.m3u8"
            ),
            Err(PlaylistError::InvalidTag {
                line: 3,
                tag: "EXT-X-MAP",
            })
        );
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"../../init.mp4\"\n\
                  #EXTINF:2,\nindex7.ts\n",
                "live/index.m3u8"
            ),
            Err(PlaylistError::InvalidUri { line: 3 })
        );
    }

    #[test]
    fn malformed_playlists_are_errors() {
        let path = "index.m3u8";
        let cases: [(&[u8], PlaylistError); 9] = [
            (b"\xff\xfe", PlaylistError::NotUtf8),
            (b"#EXTINF:2,\na.ts\n", PlaylistError::MissingHeader),
            (
                b"#EXTM3U\n#EXTINF:2,\na.ts\n",
                PlaylistError::MissingTargetDuration,
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:x\n",
                PlaylistError::InvalidTag {
                    line: 2,
                    tag: "EXT-X-TARGETDURATION",
                },
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:-1,\na.ts\n",
                PlaylistError::InvalidTag {
                    line: 3,
                    tag: "EXTINF",
                },
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\na.ts\n",
                PlaylistError::SegmentWithoutDuration { line: 3 },
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\n../../a.ts\n",
                PlaylistError::InvalidUri { line: 4 },
            ),
            (
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n",
                PlaylistError::VariantWithoutUri,
            ),
            (
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n\
                  #EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n",
                PlaylistError::SequenceOverflow,
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(parse(bytes, path), Err(expected));
        }
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nhttp://cdn/a.ts\n",
                path
            ),
            Err(PlaylistError::InvalidUri { line: 4 })
        );
        assert_eq!(
            parse(
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n",
                path
            ),
            Err(PlaylistError::MixedMasterAndMedia)
        );
    }

    #[test]
    fn object_paths_normalize_inside_the_key_namespace() {
        assert_eq!(normalize_path("a/./b//c.ts").as_deref(), Some("a/b/c.ts"));
        assert_eq!(normalize_path("a/../b.ts").as_deref(), Some("b.ts"));
        assert_eq!(normalize_path("../b.ts"), None);
        assert_eq!(normalize_path("a\\b.ts"), None);
        assert_eq!(normalize_path("a\0.ts"), None);
        assert_eq!(normalize_path("/"), None);
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse(&bytes, "v/index.m3u8");
        }

        #[test]
        fn arbitrary_tag_soup_never_panics(
            lines in proptest::collection::vec(
                prop_oneof![
                    Just("#EXTM3U".to_string()),
                    "#EXT-X-TARGETDURATION:[-0-9.e]{0,8}",
                    "#EXT-X-MEDIA-SEQUENCE:[0-9]{0,22}",
                    "#EXTINF:[-0-9.,a-z]{0,10}",
                    Just("#EXT-X-DISCONTINUITY".to_string()),
                    Just("#EXT-X-ENDLIST".to_string()),
                    "#EXT-X-STREAM-INF:[A-Z=0-9,\"]{0,24}",
                    "#EXT-X-MEDIA:[A-Z=0-9,\"/.]{0,32}",
                    "#EXT-X-MAP:[A-Z=0-9,\"]{0,24}",
                    "[./a-z0-9?#]{0,12}",
                ],
                0..40,
            )
        ) {
            let _ = parse(lines.join("\n").as_bytes(), "a/b/index.m3u8");
        }
    }
}
