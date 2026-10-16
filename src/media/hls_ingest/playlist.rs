//! A small m3u8 parser for HLS push ingest. Pure: bytes in, a playlist or an
//! error out. Malformed input is an error, never a panic.
//!
//! It reads what ordering needs: `#EXTM3U`, `EXT-X-TARGETDURATION`,
//! `EXT-X-MEDIA-SEQUENCE`, `EXTINF`, `EXT-X-DISCONTINUITY`, `EXT-X-MAP`,
//! `EXT-X-ENDLIST` and master playlists (`EXT-X-STREAM-INF` with
//! `BANDWIDTH`). Every other tag is ignored. URIs resolve against the
//! playlist's own path; only relative URIs that stay inside the stream key's
//! namespace are accepted.

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
    /// The current `EXT-X-MAP` init URI, resolved against the playlist
    /// directory like a segment URI; the last `EXT-X-MAP` wins, because
    /// that is the map the segments that follow it use.
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterPlaylist {
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub bandwidth: u64,
    pub path: String,
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
    let mut segment_uris: Vec<(String, Duration, bool)> = Vec::new();
    let mut variants = Vec::new();

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
            map_uri = Some(
                resolve_uri(playlist_path, uri)
                    .ok_or(PlaylistError::InvalidUri { line: line_number })?,
            );
        } else if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bandwidth = attribute(attributes, "BANDWIDTH")
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid("EXT-X-STREAM-INF"))?;
            pending_bandwidth = Some(bandwidth);
        } else if line.starts_with('#') {
            // Comments and tags this ingest does not need.
        } else {
            let path = resolve_uri(playlist_path, line)
                .ok_or(PlaylistError::InvalidUri { line: line_number })?;
            if let Some(bandwidth) = pending_bandwidth.take() {
                variants.push(Variant { bandwidth, path });
            } else {
                let duration = pending_duration
                    .take()
                    .ok_or(PlaylistError::SegmentWithoutDuration { line: line_number })?;
                segment_uris.push((path, duration, std::mem::take(&mut pending_discontinuity)));
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
        return Ok(Playlist::Master(MasterPlaylist { variants }));
    }

    let target_duration = target_duration.ok_or(PlaylistError::MissingTargetDuration)?;
    let mut segments = Vec::with_capacity(segment_uris.len());
    for (offset, (path, duration, discontinuity)) in segment_uris.into_iter().enumerate() {
        let sequence = u64::try_from(offset)
            .ok()
            .and_then(|offset| media_sequence.checked_add(offset))
            .ok_or(PlaylistError::SequenceOverflow)?;
        segments.push(SegmentEntry {
            sequence,
            path,
            duration,
            discontinuity,
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
                },
                SegmentEntry {
                    sequence: 8,
                    path: "other/index8.ts".into(),
                    duration: Duration::from_secs_f64(1.5),
                    discontinuity: true,
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
                path: "high/index.m3u8".into()
            })
        );
        assert_eq!(master.variants[0].path, "low/index.m3u8");
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
