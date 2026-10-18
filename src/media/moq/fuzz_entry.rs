//! Entry points for the cargo-fuzz targets in `fuzz/` (built only with
//! `--cfg fuzzing`). Each runs a production MoQ parser on untrusted bytes
//! the way a session feeds it; the parsers stay private to `media::moq`.

use bytes::Bytes;
use futures_util::FutureExt;
use super::ingest::media::TrackSet;
use super::media::from_hang_catalog;

/// A publisher's hang or MSF catalog and LOC / legacy / CMAF frames,
/// through the same upstream catalog consumers and production conversion
/// as ingest. Frame records are a track byte, big-endian u16 length, payload.
pub fn ingest(catalog: &[u8], frames: &[u8]) {
    // Match the network model's per-frame cap before allocating fuzz input.
    if catalog.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
        return;
    }
    if let Ok(catalog) = hang::Catalog::<()>::from_slice(catalog) {
        convert_frames(TrackSet::new(from_hang_catalog(&catalog)), frames);
    }
    // The MSF -> hang mapping is private upstream. Feed its real consumer a
    // finished catalog publication, just as the network model supplies it.
    // Everything is ready synchronously; no runtime or spawned task is needed.
    let broadcast = moq_net::broadcast::Info::new().produce();
    let Ok(track) = broadcast.create_track(moq_msf::DEFAULT_NAME, hang::Catalog::default_track_info())
    else {
        return;
    };
    let subscriber = track.subscribe(hang::Catalog::default_subscription());
    let Ok(mut group) = track.append_group() else {
        return;
    };
    if group
        .write_frame(moq_net::Timestamp::ZERO, Bytes::copy_from_slice(catalog))
        .and_then(|()| group.finish())
        .is_err()
    {
        return;
    }
    let mut consumer = moq_mux::catalog::msf::Consumer::<()>::new(subscriber);
    if let Some(Ok(Some(catalog))) = consumer.next().now_or_never() {
        convert_frames(TrackSet::new(from_hang_catalog(&catalog)), frames);
    }
}

fn convert_frames(mut tracks: TrackSet, mut frames: &[u8]) {
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
