//! HLS push ingest, fMP4: the top-level box walker, the init segment
//! (`handle_init_segment` and the codec scope) and media segments
//! (`handle_media_segment` per fragment, sample conversion) on untrusted
//! bytes uploaded by any stream key holder. The first two bytes are the init
//! segment's length (big-endian); the rest is one media segment, fed in
//! chunks as a session publishes it, each followed by the empty-chunk steps
//! its pending work takes.
#![no_main]

use bytes::Bytes;
use restream_engine::ingest_publisher::SegmentDemux;
use restream_engine::ingest_publisher::fmp4::{Fmp4Demux, boxes};

const MAX_INIT_BYTES: usize = 4 * 1024 * 1024;
const CHUNK: usize = 4096;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((length, rest)) = data.split_first_chunk::<2>() else {
        return;
    };
    let init_len = usize::from(u16::from_be_bytes(*length)).min(rest.len());
    let (init, media) = rest.split_at(init_len);

    let mut fragments = Vec::new();
    let _ = boxes::media_fragments(data, &mut fragments);
    let _ = boxes::is_init_segment(init);
    let _ = Fmp4Demux::check_init(init, MAX_INIT_BYTES);

    let mut demux = Fmp4Demux::default();
    if demux.apply_init(init, MAX_INIT_BYTES).is_err() {
        return;
    }
    let _ = demux.take_probe();
    let mut out = Vec::new();
    let media = Bytes::copy_from_slice(media);
    if demux.begin_object(&media).is_ok() {
        'object: for chunk in media.chunks(CHUNK) {
            if demux.feed(chunk).is_err() {
                break;
            }
            demux.drain_into(&mut out);
            while demux.pending() {
                if demux.feed(&[]).is_err() {
                    break 'object;
                }
                demux.drain_into(&mut out);
            }
        }
        let _ = demux.end_object();
    }
    demux.flush();
    demux.drain_into(&mut out);
});
