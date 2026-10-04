// Test code may index, unwrap and do plain arithmetic; the parent module's
// deny lints guard the parser, not its tests.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use proptest::prelude::*;

use super::*;

fn read(bytes: &[u8]) -> Result<Option<Response>, HttpError> {
    ResponseReader::new().feed(bytes)
}

fn ok(status: u16, keep_alive: bool) -> Result<Option<Response>, HttpError> {
    Ok(Some(Response { status, keep_alive }))
}

#[test]
fn request_head_is_a_persistent_http11_put_with_its_length() {
    let mut out = Vec::new();
    write_request_head(
        &RequestHead {
            method: "PUT",
            target: "/http_upload_hls?cid=k&copy=0&file=r1-0.ts",
            host: "a.upload.youtube.com",
            user_agent: "Restream / restream / 1.0",
            content_type: "video/mp2t",
            content_length: 1316,
        },
        &mut out,
    );
    assert_eq!(
        out,
        b"PUT /http_upload_hls?cid=k&copy=0&file=r1-0.ts HTTP/1.1\r\nHost: a.upload.youtube.com\r\nUser-Agent: Restream / restream / 1.0\r\nContent-Type: video/mp2t\r\nContent-Length: 1316\r\n\r\n"
    );
}

#[test]
fn every_body_framing_reads_to_one_response() {
    // Content-Length body, 202 (YouTube: segment before its playlist).
    assert_eq!(
        read(b"HTTP/1.1 202 Accepted\r\nContent-Length: 5\r\n\r\nhello"),
        ok(202, true)
    );
    // No body by status.
    assert_eq!(read(b"HTTP/1.1 204 No Content\r\n\r\n"), ok(204, true));
    // Chunked, with an extension and a trailer.
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;ext=1\r\nabc\r\n0\r\nX-T: 1\r\n\r\n"),
        ok(200, true)
    );
    // Transfer-Encoding wins over Content-Length (RFC 9112 6.3).
    assert_eq!(
        read(
            b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
        ),
        ok(200, true)
    );
    // Connection: close, and HTTP/1.0 without keep-alive.
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"),
        ok(200, false)
    );
    assert_eq!(
        read(b"HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n"),
        ok(200, false)
    );
    assert_eq!(
        read(b"HTTP/1.0 200 OK\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n"),
        ok(200, true)
    );
    // Interim 100 Continue before the real response.
    assert_eq!(
        read(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n"),
        ok(201, true)
    );
}

#[test]
fn a_body_without_length_ends_at_close() {
    let mut reader = ResponseReader::new();
    assert_eq!(
        reader.feed(b"HTTP/1.1 500 Oops\r\n\r\npartial body"),
        Ok(None)
    );
    assert_eq!(
        reader.finish(),
        Ok(Response {
            status: 500,
            keep_alive: false
        })
    );
}

#[test]
fn incomplete_or_hostile_responses_are_errors() {
    let mut truncated = ResponseReader::new();
    assert_eq!(
        truncated.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"),
        Ok(None)
    );
    assert_eq!(truncated.finish(), Err(HttpError::Truncated));

    assert!(matches!(
        read(b"NOT HTTP\r\n\r\n"),
        Err(HttpError::Malformed(_))
    ));
    assert!(matches!(
        read(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"),
        Err(HttpError::Malformed(_))
    ));
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n"),
        Err(HttpError::BodyTooLarge)
    );
    assert_eq!(
        read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFF\r\n"),
        Err(HttpError::BodyTooLarge)
    );
    assert!(matches!(
        read(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabc\r\n"),
        Err(HttpError::Malformed(_))
    ));
    let mut huge = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
    huge.resize(MAX_RESPONSE_HEAD + 64, b'a');
    assert_eq!(read(&huge), Err(HttpError::HeadTooLarge));
}

fn well_formed() -> impl Strategy<Value = (Vec<u8>, Response)> {
    let body = prop::collection::vec(any::<u8>(), 0..64);
    (
        prop::sample::select(vec![200u16, 201, 202, 204, 400, 401, 503]),
        body,
        0u8..3,
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(status, body, framing, close, interim)| {
            let mut bytes = Vec::new();
            if interim {
                bytes.extend_from_slice(b"HTTP/1.1 100 Continue\r\n\r\n");
            }
            bytes.extend_from_slice(format!("HTTP/1.1 {status} X\r\n").as_bytes());
            if close {
                bytes.extend_from_slice(b"Connection: close\r\n");
            }
            let body = if status == 204 { Vec::new() } else { body };
            match framing {
                0 => {
                    bytes.extend_from_slice(
                        format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes(),
                    );
                    bytes.extend_from_slice(&body);
                }
                _ => {
                    bytes.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
                    for chunk in body.chunks(7) {
                        bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                        bytes.extend_from_slice(chunk);
                        bytes.extend_from_slice(b"\r\n");
                    }
                    bytes.extend_from_slice(b"0\r\n\r\n");
                }
            }
            (
                bytes,
                Response {
                    status,
                    keep_alive: !close,
                },
            )
        })
}

proptest! {
    /// A response split at any points reads the same as one feed.
    #[test]
    fn any_split_reads_the_same_response(
        (bytes, expected) in well_formed(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        let mut points: Vec<usize> = cuts.iter().map(|cut| cut.index(bytes.len() + 1)).collect();
        points.push(0);
        points.push(bytes.len());
        points.sort_unstable();
        let mut reader = ResponseReader::new();
        let mut result = None;
        for window in points.windows(2) {
            if let Some(response) = reader.feed(&bytes[window[0]..window[1]]).unwrap() {
                result = Some(response);
            }
        }
        prop_assert_eq!(result, Some(expected));
    }

    #[test]
    fn arbitrary_bytes_never_panic(chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 0..8)) {
        let mut reader = ResponseReader::new();
        for chunk in &chunks {
            if reader.feed(chunk).is_err() {
                break;
            }
        }
        let _ = reader.finish();
    }
}
