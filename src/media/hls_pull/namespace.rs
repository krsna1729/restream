//! The push-ingest namespace a remote playlist is rewritten into, where the
//! pull enters a live window, and the playlists it refuses.
//!
//! URIs in a remote playlist are absolute URLs, while the assembler parses
//! upload paths inside one stream key's namespace, so sequence `N` becomes
//! `pull/sN.ts` (or `.m4s`) and an `EXT-X-MAP` URI becomes a stable
//! `pull/m<hash>.mp4` ([`map_name`]). A fetched playlist is parsed and
//! prepared with [`parse_and_prepare`] — which refuses what this ingest cannot
//! fetch, including a private target — and rewritten with
//! [`push_playlist_text`].

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use reqwest::Url;

use super::PullError;
use super::fetch::{host_refused, log_url};
use crate::media::hls_ingest::ObjectKind;
use crate::media::hls_ingest::playlist::{
    self, Encryption, MasterPlaylist, MediaPlaylist, Playlist, SegmentEntry,
};

/// The container a pulled segment carries. A playlist that names an
/// `EXT-X-MAP` carries fMP4 media; otherwise the URL's extension decides,
/// and an unknown extension is tried as MPEG-TS (the assembler refuses it
/// with a reason if that is wrong).
pub(super) fn object_kind(segment: &SegmentEntry) -> ObjectKind {
    if segment.map_uri.is_some() {
        return ObjectKind::Fmp4;
    }
    let path = segment.path.split(['?', '#']).next().unwrap_or_default();
    ObjectKind::of(path).unwrap_or(ObjectKind::TsSegment)
}

pub(super) fn segment_name(sequence: u64, kind: ObjectKind) -> String {
    match kind {
        ObjectKind::Fmp4 => format!("s{sequence}.m4s"),
        ObjectKind::Playlist | ObjectKind::TsSegment => format!("s{sequence}.ts"),
    }
}

/// A stable object name for one `EXT-X-MAP` init, so the map's identity
/// survives playlist reloads. The identity is scheme, host and path: a signed
/// origin (Akamai `hdnts`, CloudFront) re-signs the same init URI on every
/// playlist response, and the query is the credential, not the identity. A
/// name that followed the query would refetch the init and hand the assembler
/// the same map under a new name on every reload.
pub(super) fn map_name(url: &str) -> String {
    let identity = match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.split(['?', '#']).next().unwrap_or(url).to_string(),
    };
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    format!("m{:016x}.mp4", hasher.finish())
}

/// The first sequence a live playlist is followed from: the segment closest
/// to the end whose start is still at least three target durations before the
/// playlist ends, or the first segment of a window shorter than that
/// (RFC 8216 section 6.3.3). Entering at that edge is what keeps a DVR or
/// EVENT window from being read from its start and paced in real time, and
/// what keeps the whole window off the origin.
///
/// The distance is measured from the chosen segment's start to the end of the
/// last listed segment, so the segment itself counts: three 6 s segments fill
/// three 6 s target durations exactly, and the earliest of them is the one a
/// client enters at.
pub(super) fn live_start(media: &MediaPlaylist) -> u64 {
    let horizon = media.target_duration.saturating_mul(3);
    let mut covered = Duration::ZERO;
    let mut start = media.media_sequence;
    for segment in media.segments.iter().rev() {
        covered = covered.saturating_add(segment.duration);
        start = segment.sequence;
        if covered >= horizon {
            break;
        }
    }
    start
}

/// A parsed playlist, prepared for the pull: a media playlist carries the text
/// the ingest reads, a master playlist carries nothing because the pull has yet
/// to choose a variant from it.
#[derive(Debug)]
pub(super) enum Prepared {
    Media(MediaPlaylist, String),
    Master(MasterPlaylist),
}

/// Parse a playlist fetched from a remote origin, refuse what this pull cannot
/// fetch, and render the text the ingest will read.
///
/// All of it belongs together and off the caller's worker: each pass is linear
/// in the playlist (a 1 MiB playlist is ~10 000 segments), and the private-host
/// question is decided from the `Url` the resolver has already built — parsing
/// each rendered URI a second time to ask it again was avoidable work.
pub(super) fn parse_and_prepare(
    base: &Url,
    bytes: &[u8],
    allow_private: bool,
) -> Result<Prepared, PullError> {
    let mut refused: Option<String> = None;
    let parsed = playlist::parse_with(bytes, |uri| {
        let mut url = base.join(uri).ok()?;
        // The fragment never reaches the origin.
        url.set_fragment(None);
        if !allow_private && host_refused(&url) && refused.is_none() {
            refused = Some(log_url(url.as_ref()));
        }
        Some(url.to_string())
    })
    .map_err(|error| PullError::Retry(format!("HLS playlist does not parse: {error}")))?;
    if let Some(target) = refused {
        return Err(PullError::Fatal(format!(
            "HLS pull refuses a playlist that points at a private address: {target}"
        )));
    }
    Ok(match parsed {
        Playlist::Media(media) => {
            refuse_unfetchable(&media)?;
            let text = push_playlist_text(&media);
            Prepared::Media(media, text)
        }
        Playlist::Master(master) => {
            if let Some(encryption) = &master.session_key {
                return Err(encryption_refusal(encryption));
            }
            Prepared::Master(master)
        }
    })
}

/// Why an encrypted playlist is not fetched, in one line. The method is the
/// tag's own text, or `<absent>` for a tag that names none — the RFC requires
/// one, and the parser records such a tag rather than refusing the playlist,
/// so a push upload goes on reading what it always read.
pub(super) fn encryption_refusal(encryption: &Encryption) -> PullError {
    let method = if encryption.method.is_empty() {
        "<absent>"
    } else {
        encryption.method.as_str()
    };
    PullError::Fatal(format!(
        "HLS pull refuses an encrypted playlist: EXT-X-KEY METHOD={method}{} — \
         an AES-128 segment fails per segment and a SAMPLE-AES one demuxes as \
         garbage, so this ingest cannot publish it",
        if encryption.session {
            " (session key)"
        } else {
            ""
        }
    ))
}

/// Refuse a playlist the pull cannot fetch correctly. Push shares this
/// parser and has always ignored these tags, so the refusal lives here, in
/// the pull, and not in the parser the two share.
pub(super) fn refuse_unfetchable(media: &MediaPlaylist) -> Result<(), PullError> {
    if let Some(encryption) = &media.encryption {
        return Err(encryption_refusal(encryption));
    }
    if media.byte_range {
        return Err(PullError::Fatal(
            "HLS pull refuses a byte-range playlist: EXT-X-BYTERANGE makes an \
             entry a slice of a resource, and this pull fetches whole objects"
                .to_string(),
        ));
    }
    Ok(())
}

/// Rewrite a remote media playlist into the push namespace: sequence `N` is
/// `sN.ts`/`sN.m4s`, the map is its [`map_name`], and every ordering-relevant
/// tag survives. URIs are relative to [`PLAYLIST_PATH`](super::PLAYLIST_PATH)'s directory.
pub(super) fn push_playlist_text(media: &MediaPlaylist) -> String {
    let mut text = String::with_capacity(256 + media.segments.len() * 40);
    let _ = writeln!(text, "#EXTM3U");
    let _ = writeln!(
        text,
        "#EXT-X-TARGETDURATION:{}",
        media.target_duration.as_secs_f64()
    );
    let _ = writeln!(text, "#EXT-X-MEDIA-SEQUENCE:{}", media.media_sequence);
    let mut current_map: Option<String> = None;
    for segment in &media.segments {
        if segment.discontinuity {
            let _ = writeln!(text, "#EXT-X-DISCONTINUITY");
        }
        if let Some(map) = &segment.map_uri {
            let name = map_name(map);
            if current_map.as_deref() != Some(name.as_str()) {
                let _ = writeln!(text, "#EXT-X-MAP:URI=\"{name}\"");
                current_map = Some(name);
            }
        }
        let _ = writeln!(text, "#EXTINF:{},", segment.duration.as_secs_f64());
        let _ = writeln!(
            text,
            "{}",
            segment_name(segment.sequence, object_kind(segment))
        );
    }
    if media.ended {
        let _ = writeln!(text, "#EXT-X-ENDLIST");
    }
    text
}
