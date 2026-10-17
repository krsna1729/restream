//! Top-level box walker for fMP4 objects uploaded by an HLS encoder.
//! Untrusted bytes: checked arithmetic, no indexing, no panics.
//!
//! A media segment is any run of top-level boxes with one or more
//! `moof` + `mdat` pairs. FFmpeg writes `styp sidx moof mdat`; CMAF adds
//! `prft`, `emsg` and boxes not yet invented, so everything that is not a
//! fragment is skipped. Each fragment is handed to the demuxer on its own,
//! which is why an explicit `tfhd` `base_data_offset` is refused: it counts
//! from the start of the whole segment, not of the fragment. Every `traf`
//! must carry a `tfdt` for the same reason: without it a fragment has no
//! place on the stream's timeline.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::ops::Range;

/// Most `moof` + `mdat` pairs one media segment may carry; a hostile segment
/// cannot force unbounded demux work.
pub const MAX_FRAGMENTS: usize = 64;

/// Top-level boxes one media segment may carry. The walk runs inside a poll
/// on a shared MEDIA executor, so a segment of tiny boxes must not be able to
/// hold a worker for millions of iterations: past this it is refused.
pub const MAX_BOXES: usize = 4096;

/// `tfhd` flag: `base_data_offset` is present.
const TFHD_BASE_DATA_OFFSET_PRESENT: u32 = 0x00_0001;

/// Top-level boxes a media segment may carry besides its fragments.
const KNOWN_SKIPPED: [[u8; 4]; 8] = [
    *b"styp", *b"sidx", *b"ssix", *b"prft", *b"emsg", *b"free", *b"skip", *b"ftyp",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkError {
    /// A box header runs past the end of the object.
    Truncated {
        offset: usize,
    },
    /// A box declares a size below its own header.
    TooSmall {
        kind: [u8; 4],
        offset: usize,
    },
    /// A box declares a size beyond the end of the object.
    Oversized {
        kind: [u8; 4],
        offset: usize,
    },
    /// `moof` or `mdat` with size 0 ("to the end"), which is ambiguous there.
    SizeToEnd {
        kind: [u8; 4],
    },
    MoofWithoutMdat {
        offset: usize,
    },
    MdatWithoutMoof {
        offset: usize,
    },
    NoFragments,
    TooManyFragments,
    /// More top-level boxes than [`MAX_BOXES`]: the walk is per poll.
    TooManyBoxes,
    /// A `moof` whose `traf`s set an explicit `base_data_offset`.
    ExplicitBaseDataOffset,
    /// A `traf` has no `tfdt`, so the fragment carries no timeline.
    TrafWithoutTfdt,
    NoMoov,
}

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { offset } => write!(f, "box header truncated at byte {offset}"),
            Self::TooSmall { kind, offset } => {
                write!(
                    f,
                    "box {} at byte {offset} is smaller than its header",
                    name(kind)
                )
            }
            Self::Oversized { kind, offset } => {
                write!(
                    f,
                    "box {} at byte {offset} runs past the object",
                    name(kind)
                )
            }
            Self::SizeToEnd { kind } => write!(f, "box {} has size 0", name(kind)),
            Self::MoofWithoutMdat { offset } => {
                write!(f, "moof at byte {offset} is not followed by an mdat")
            }
            Self::MdatWithoutMoof { offset } => {
                write!(f, "mdat at byte {offset} has no moof before it")
            }
            Self::NoFragments => write!(f, "media segment has no moof"),
            Self::TooManyFragments => {
                write!(f, "media segment has more than {MAX_FRAGMENTS} fragments")
            }
            Self::TooManyBoxes => {
                write!(f, "media segment has more than {MAX_BOXES} top-level boxes")
            }
            Self::ExplicitBaseDataOffset => write!(
                f,
                "tfhd sets an explicit base_data_offset, which counts from the whole segment; \
                 only moof-relative offsets are supported"
            ),
            Self::TrafWithoutTfdt => write!(
                f,
                "traf has no tfdt: a fragment without a decode time cannot be placed on the \
                 stream's timeline"
            ),
            Self::NoMoov => write!(f, "init segment has no moov"),
        }
    }
}

impl std::error::Error for WalkError {}

/// A box type as text, for errors and logs.
pub fn name(kind: &[u8; 4]) -> String {
    kind.iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() {
                char::from(byte)
            } else {
                '?'
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoxSpan {
    kind: [u8; 4],
    start: usize,
    /// First payload byte.
    body: usize,
    end: usize,
    /// Declared size 0: the box runs to the end of its container.
    to_end: bool,
}

/// Read the box header at `offset` in `bytes`; size 1 is a 64-bit
/// `largesize` after the type, size 0 runs to the end of `bytes`.
fn read_box(bytes: &[u8], offset: usize) -> Result<BoxSpan, WalkError> {
    let truncated = WalkError::Truncated { offset };
    let header = bytes
        .get(offset..)
        .and_then(|rest| rest.first_chunk::<8>())
        .ok_or(truncated.clone())?;
    let [s0, s1, s2, s3, k0, k1, k2, k3] = *header;
    let kind = [k0, k1, k2, k3];
    let size32 = u32::from_be_bytes([s0, s1, s2, s3]);
    let (size, header_len) = match size32 {
        1 => {
            let large = offset
                .checked_add(8)
                .and_then(|at| bytes.get(at..))
                .and_then(|rest| rest.first_chunk::<8>())
                .ok_or(truncated)?;
            (u64::from_be_bytes(*large), 16u64)
        }
        size => (u64::from(size), 8u64),
    };
    let header_end = offset
        .checked_add(usize::try_from(header_len).unwrap_or(usize::MAX))
        .ok_or(WalkError::Oversized { kind, offset })?;
    if size32 == 0 {
        return Ok(BoxSpan {
            kind,
            start: offset,
            body: header_end,
            end: bytes.len(),
            to_end: true,
        });
    }
    if size < header_len {
        return Err(WalkError::TooSmall { kind, offset });
    }
    let end = usize::try_from(size)
        .ok()
        .and_then(|size| offset.checked_add(size))
        .filter(|end| *end <= bytes.len())
        .ok_or(WalkError::Oversized { kind, offset })?;
    Ok(BoxSpan {
        kind,
        start: offset,
        body: header_end,
        end,
        to_end: false,
    })
}

/// What a media segment walk found besides its fragments.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WalkReport {
    /// The first top-level box type neither a fragment nor a known skipped
    /// box (logged once per session by the caller).
    pub first_unknown: Option<[u8; 4]>,
}

/// Walk a media segment's top-level boxes and collect each `moof` + `mdat`
/// pair, as the byte range from the `moof` start to the `mdat` end, into
/// `fragments` (cleared first).
pub fn media_fragments(
    bytes: &[u8],
    fragments: &mut Vec<Range<usize>>,
) -> Result<WalkReport, WalkError> {
    fragments.clear();
    let mut report = WalkReport::default();
    let mut offset = 0usize;
    let mut walked = 0usize;
    while offset < bytes.len() {
        walked = walked.saturating_add(1);
        if walked > MAX_BOXES {
            return Err(WalkError::TooManyBoxes);
        }
        let span = read_box(bytes, offset)?;
        match &span.kind {
            b"moof" => {
                if span.to_end {
                    return Err(WalkError::SizeToEnd { kind: span.kind });
                }
                check_moof(bytes.get(span.body..span.end).unwrap_or_default())?;
                if span.end >= bytes.len() {
                    return Err(WalkError::MoofWithoutMdat { offset });
                }
                let mdat = read_box(bytes, span.end)?;
                if &mdat.kind != b"mdat" {
                    return Err(WalkError::MoofWithoutMdat { offset });
                }
                if mdat.to_end {
                    return Err(WalkError::SizeToEnd { kind: mdat.kind });
                }
                if fragments.len() >= MAX_FRAGMENTS {
                    return Err(WalkError::TooManyFragments);
                }
                fragments.push(span.start..mdat.end);
                offset = mdat.end;
                continue;
            }
            b"mdat" => return Err(WalkError::MdatWithoutMoof { offset }),
            kind if KNOWN_SKIPPED.contains(kind) => {}
            kind => {
                report.first_unknown.get_or_insert(*kind);
            }
        }
        offset = span.end;
    }
    if fragments.is_empty() {
        return Err(WalkError::NoFragments);
    }
    Ok(report)
}

/// Refuse a `moof` whose `traf`s set an explicit `base_data_offset`.
fn check_moof(moof: &[u8]) -> Result<(), WalkError> {
    for_each_child(moof, |child, body| {
        if &child.kind != b"traf" {
            return Ok(());
        }
        // Every fragment must carry its own decode time: the demuxer reads
        // `tfdt` and would otherwise place it at zero, silently gluing a
        // restarted segment's timeline onto the previous one.
        let mut has_tfdt = false;
        for_each_child(body, |child, body| match &child.kind {
            b"tfdt" => {
                has_tfdt = true;
                Ok(())
            }
            b"tfhd" => {
                let &[_version, f0, f1, f2] = body
                    .first_chunk::<4>()
                    .ok_or(WalkError::Truncated { offset: child.body })?;
                let flags = u32::from_be_bytes([0, f0, f1, f2]);
                if flags & TFHD_BASE_DATA_OFFSET_PRESENT != 0 {
                    return Err(WalkError::ExplicitBaseDataOffset);
                }
                Ok(())
            }
            _ => Ok(()),
        })?;
        if !has_tfdt {
            return Err(WalkError::TrafWithoutTfdt);
        }
        Ok(())
    })
}

/// Visit each child box of a container payload; size 0 is refused inside a
/// container (it would swallow its siblings).
fn for_each_child(
    payload: &[u8],
    mut visit: impl FnMut(&BoxSpan, &[u8]) -> Result<(), WalkError>,
) -> Result<(), WalkError> {
    let mut offset = 0usize;
    while offset < payload.len() {
        let span = read_box(payload, offset)?;
        if span.to_end {
            return Err(WalkError::SizeToEnd { kind: span.kind });
        }
        visit(&span, payload.get(span.body..span.end).unwrap_or_default())?;
        offset = span.end;
    }
    Ok(())
}

/// The byte range of an init segment's top-level `moov`.
pub fn moov_range(bytes: &[u8]) -> Result<Range<usize>, WalkError> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let span = read_box(bytes, offset)?;
        if &span.kind == b"moov" {
            return Ok(span.start..span.end);
        }
        offset = span.end;
    }
    Err(WalkError::NoMoov)
}

/// Whether an object is an init segment: a top-level `moov` comes before
/// any `moof`. Malformed objects are not inits; the media walk reports them.
pub fn is_init_segment(bytes: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let Ok(span) = read_box(bytes, offset) else {
            return false;
        };
        match &span.kind {
            b"moov" => return true,
            b"moof" => return false,
            _ => offset = span.end,
        }
    }
    false
}

#[cfg(test)]
#[path = "boxes_tests.rs"]
mod tests;
