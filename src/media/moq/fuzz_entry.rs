//! Entry points for the cargo-fuzz targets in `fuzz/` (built only with
//! `--cfg fuzzing`). Each runs a production MoQ parser on untrusted bytes
//! the way a session feeds it; the parsers stay private to `media::moq`.

use bytes::Bytes;

use super::ingest::media::TrackSet;
use super::media::from_hang_catalog;

/// A publisher's `catalog.json` and frames, as an ingest session reads
/// them: the catalog becomes the track set, then every frame goes to the
/// track it names. `frames` is a sequence of one byte (the track, modulo the
/// accepted tracks), a 2-byte big-endian length and that many bytes. A frame
/// the converter refuses is skipped, as one malformed frame is in a session.
pub fn ingest(catalog: &[u8], mut frames: &[u8]) {
    let Ok(catalog) = hang::Catalog::<()>::from_slice(catalog) else {
        return;
    };
    let mut tracks = TrackSet::new(from_hang_catalog(&catalog));
    let _ = tracks.probe();
    let count = tracks.tracks().len();
    if count == 0 {
        return;
    }
    let mut packets = Vec::new();
    while let Some((&[track, high, low], rest)) = frames.split_first_chunk::<3>() {
        let len = usize::from(u16::from_be_bytes([high, low])).min(rest.len());
        let (frame, next) = rest.split_at(len);
        frames = next;
        if let Some(track) = tracks.track_mut(usize::from(track) % count) {
            let _ = track.push(Bytes::copy_from_slice(frame), &mut packets);
        }
        packets.clear();
    }
}
