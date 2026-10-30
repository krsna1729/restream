//! MoQ ingest: a publisher's hang `catalog.json` (track selection, hex
//! decoder configurations, AVCC/HVCC and AudioSpecificConfig parsing) and
//! its frames (legacy varint timestamps, length-prefixed or Annex B NAL
//! units, AAC, CMAF fragments) on untrusted bytes sent by any stream key
//! holder. The first two bytes are the catalog's length (big-endian); the
//! rest is frames: one byte naming the track, a 2-byte big-endian length,
//! the frame.
#![no_main]

use restream_engine::moq_fuzz_entry;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((length, rest)) = data.split_first_chunk::<2>() else {
        return;
    };
    let catalog_len = usize::from(u16::from_be_bytes(*length)).min(rest.len());
    let (catalog, frames) = rest.split_at(catalog_len);
    moq_fuzz_entry::ingest(catalog, frames);
});
