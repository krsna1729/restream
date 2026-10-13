//! Entry points for the cargo-fuzz targets in `fuzz/` (built only with
//! `--cfg fuzzing`). Each one runs a production egress parser on untrusted
//! bytes exactly as the shard calls it; the parsers stay private to
//! `media::egress`.

use super::http1::{HttpError, Response, ResponseReader};

/// Feed `reads` one by one, as the HLS PUT shard does, and close the
/// connection after the last read if the response is not yet complete.
fn read_response<'a>(reads: impl IntoIterator<Item = &'a [u8]>) -> Result<Response, HttpError> {
    let mut reader = ResponseReader::new();
    for read in reads {
        if let Some(response) = reader.feed(read)? {
            return Ok(response);
        }
    }
    reader.finish()
}

/// Bytes from an HTTP server after an HLS PUT request, delivered in the
/// given reads. Whether a response is accepted, and which, must not depend on
/// where the reads split it. The error kind may: a head still incomplete past
/// its size limit is `HeadTooLarge` when split, but the whole input can show
/// httparse a bad byte first.
pub fn http_response<'a>(reads: impl IntoIterator<Item = &'a [u8]> + Clone) {
    let split = read_response(reads.clone());
    let whole: Vec<u8> = reads.into_iter().flatten().copied().collect();
    let joined = read_response([whole.as_slice()]);
    assert_eq!(
        split.ok(),
        joined.ok(),
        "the response depends on how reads split it"
    );
}
