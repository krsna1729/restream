//! HTTP/1.1 client codec for egress uploads, sans-I/O: the request head an
//! upload writes before its body, and a reader that turns response bytes,
//! fed in any split, into one complete response. Bodies are counted and
//! discarded; an upload needs only the status and whether the connection
//! stays open.
//!
//! Responses are untrusted peer bytes: every size is bounded, and a malformed
//! response is an error, never a panic.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::io::Write as _;

/// Status line plus headers.
pub(crate) const MAX_RESPONSE_HEAD: usize = 16 * 1024;
/// A larger body is not an upload acknowledgement.
pub(crate) const MAX_RESPONSE_BODY: u64 = 1024 * 1024;
const MAX_HEADERS: usize = 64;
/// A chunk-size line: hex size, optional extensions, CRLF.
const MAX_CHUNK_LINE: usize = 1024;

/// The head of one upload request. The body follows it on the wire.
pub(crate) struct RequestHead<'a> {
    pub(crate) method: &'static str,
    /// Origin-form target: path plus `?query`, as it should appear on the
    /// wire (YouTube: `file=` must not be URL-encoded).
    pub(crate) target: &'a str,
    /// `host` or `host:port` (port omitted when it is the scheme default).
    pub(crate) host: &'a str,
    pub(crate) user_agent: &'a str,
    pub(crate) content_type: &'a str,
    pub(crate) content_length: usize,
}

/// Append `head` to `out`. The connection is persistent (HTTP/1.1 default),
/// as YouTube and Akamai require.
pub(crate) fn write_request_head(head: &RequestHead<'_>, out: &mut Vec<u8>) {
    // Writing to a Vec cannot fail.
    let _ = write!(
        out,
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
        head.method,
        head.target,
        head.host,
        head.user_agent,
        head.content_type,
        head.content_length
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// The server keeps the connection open for the next request.
    pub(crate) keep_alive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpError {
    Malformed(&'static str),
    HeadTooLarge,
    BodyTooLarge,
    /// The connection closed before the response was complete.
    Truncated,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed HTTP response: {what}"),
            Self::HeadTooLarge => write!(f, "HTTP response head over {MAX_RESPONSE_HEAD} bytes"),
            Self::BodyTooLarge => write!(f, "HTTP response body over {MAX_RESPONSE_BODY} bytes"),
            Self::Truncated => write!(f, "connection closed mid-response"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum State {
    Head,
    /// `remaining` body bytes to discard.
    Sized {
        response: Response,
        remaining: u64,
    },
    /// Chunked body: next is a chunk-size line.
    ChunkSize {
        response: Response,
        total: u64,
    },
    /// Chunked body: discarding chunk data.
    ChunkData {
        response: Response,
        total: u64,
        remaining: u64,
    },
    /// Chunked body: the CRLF after chunk data.
    ChunkDataEnd {
        response: Response,
        total: u64,
    },
    /// Chunked body: trailer lines until an empty line.
    Trailers {
        response: Response,
    },
    /// No length: the body runs until the peer closes.
    UntilClose {
        response: Response,
        total: u64,
    },
    Done(Response),
}

/// Reads one response. Feed it every received byte, split anywhere; it
/// reports the response once complete.
#[derive(Debug)]
pub(crate) struct ResponseReader {
    state: State,
    /// Received bytes not yet consumed: at most an incomplete head or line
    /// plus the latest feed.
    unread: Vec<u8>,
}

impl ResponseReader {
    /// Bytes received past the response (the peer wrote more than asked).
    pub(crate) fn has_unread(&self) -> bool {
        !self.unread.is_empty()
    }
}

impl Default for ResponseReader {
    fn default() -> Self {
        Self::new()
    }
}

/// What one step did with the unread bytes.
enum Step {
    /// Consumed this many bytes; the state may have changed.
    Consumed(usize),
    /// Needs more bytes.
    Wait,
}

impl ResponseReader {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Head,
            unread: Vec::new(),
        }
    }

    /// Consume `bytes`; `Some` once the whole response has been read.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Option<Response>, HttpError> {
        let mut unread = std::mem::take(&mut self.unread);
        unread.extend_from_slice(bytes);
        let mut offset = 0usize;
        let result = loop {
            if let State::Done(response) = self.state {
                break Ok(Some(response));
            }
            match self.step(unread.get(offset..).unwrap_or_default()) {
                Ok(Step::Consumed(count)) => offset = offset.saturating_add(count),
                Ok(Step::Wait) => break Ok(None),
                Err(error) => break Err(error),
            }
        };
        unread.drain(..offset.min(unread.len()));
        self.unread = unread;
        result
    }

    /// The peer closed the connection.
    pub(crate) fn finish(&mut self) -> Result<Response, HttpError> {
        match self.state {
            State::Done(response) => Ok(response),
            State::UntilClose { response, .. } => {
                let response = Response {
                    keep_alive: false,
                    ..response
                };
                self.state = State::Done(response);
                Ok(response)
            }
            _ => Err(HttpError::Truncated),
        }
    }

    fn step(&mut self, unread: &[u8]) -> Result<Step, HttpError> {
        match self.state {
            State::Head => self.read_head(unread),
            State::Sized {
                response,
                remaining,
            } => {
                let taken = take_up_to(unread, remaining);
                if taken == 0 {
                    return Ok(Step::Wait);
                }
                let remaining = remaining.saturating_sub(taken as u64);
                self.state = if remaining == 0 {
                    State::Done(response)
                } else {
                    State::Sized {
                        response,
                        remaining,
                    }
                };
                Ok(Step::Consumed(taken))
            }
            State::ChunkSize { response, total } => {
                let Some((line, consumed)) = line(unread, MAX_CHUNK_LINE)? else {
                    return Ok(Step::Wait);
                };
                let size = parse_chunk_size(line)?;
                let total = total
                    .checked_add(size)
                    .filter(|total| *total <= MAX_RESPONSE_BODY)
                    .ok_or(HttpError::BodyTooLarge)?;
                self.state = if size == 0 {
                    State::Trailers { response }
                } else {
                    State::ChunkData {
                        response,
                        total,
                        remaining: size,
                    }
                };
                Ok(Step::Consumed(consumed))
            }
            State::ChunkData {
                response,
                total,
                remaining,
            } => {
                let taken = take_up_to(unread, remaining);
                if taken == 0 {
                    return Ok(Step::Wait);
                }
                let remaining = remaining.saturating_sub(taken as u64);
                self.state = if remaining == 0 {
                    State::ChunkDataEnd { response, total }
                } else {
                    State::ChunkData {
                        response,
                        total,
                        remaining,
                    }
                };
                Ok(Step::Consumed(taken))
            }
            State::ChunkDataEnd { response, total } => {
                let Some((line, consumed)) = line(unread, 0)? else {
                    return Ok(Step::Wait);
                };
                if !line.is_empty() {
                    return Err(HttpError::Malformed("chunk data longer than its size"));
                }
                self.state = State::ChunkSize { response, total };
                Ok(Step::Consumed(consumed))
            }
            State::Trailers { response } => {
                let Some((line, consumed)) = line(unread, MAX_RESPONSE_HEAD)? else {
                    return Ok(Step::Wait);
                };
                if line.is_empty() {
                    self.state = State::Done(response);
                }
                Ok(Step::Consumed(consumed))
            }
            State::UntilClose { response, total } => {
                if unread.is_empty() {
                    return Ok(Step::Wait);
                }
                let total = u64::try_from(unread.len())
                    .ok()
                    .and_then(|len| total.checked_add(len))
                    .filter(|total| *total <= MAX_RESPONSE_BODY)
                    .ok_or(HttpError::BodyTooLarge)?;
                self.state = State::UntilClose { response, total };
                Ok(Step::Consumed(unread.len()))
            }
            State::Done(_) => Ok(Step::Wait),
        }
    }

    fn read_head(&mut self, unread: &[u8]) -> Result<Step, HttpError> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut parsed = httparse::Response::new(&mut headers);
        let head_len = match parsed.parse(unread) {
            Ok(httparse::Status::Complete(len)) => len,
            Ok(httparse::Status::Partial) => {
                if unread.len() > MAX_RESPONSE_HEAD {
                    return Err(HttpError::HeadTooLarge);
                }
                return Ok(Step::Wait);
            }
            Err(httparse::Error::TooManyHeaders) => return Err(HttpError::HeadTooLarge),
            Err(_) => return Err(HttpError::Malformed("status line or header")),
        };
        if head_len > MAX_RESPONSE_HEAD {
            return Err(HttpError::HeadTooLarge);
        }
        let status = parsed.code.ok_or(HttpError::Malformed("no status"))?;
        let http10 = parsed.version == Some(0);
        let framing = Framing::from_headers(parsed.headers, http10)?;
        if (100..200).contains(&status) {
            // 100 Continue and other interim responses precede the real one.
            return Ok(Step::Consumed(head_len));
        }
        let response = Response {
            status,
            keep_alive: framing.keep_alive,
        };
        self.state = if status == 204 || status == 304 {
            State::Done(response)
        } else {
            match framing.body {
                Body::Length(0) => State::Done(response),
                Body::Length(length) if length > MAX_RESPONSE_BODY => {
                    return Err(HttpError::BodyTooLarge);
                }
                Body::Length(length) => State::Sized {
                    response,
                    remaining: length,
                },
                Body::Chunked => State::ChunkSize { response, total: 0 },
                Body::UntilClose => State::UntilClose {
                    response: Response {
                        keep_alive: false,
                        ..response
                    },
                    total: 0,
                },
            }
        };
        Ok(Step::Consumed(head_len))
    }
}

/// The CRLF-terminated line at the front of `unread` (without its CRLF) and
/// the bytes it spans; `None` until complete. A line longer than `max` is an
/// error.
fn line(unread: &[u8], max: usize) -> Result<Option<(&[u8], usize)>, HttpError> {
    let Some(end) = unread.windows(2).position(|pair| pair == b"\r\n") else {
        if unread.len() > max.saturating_add(1) {
            return Err(HttpError::Malformed("line too long"));
        }
        return Ok(None);
    };
    if end > max {
        return Err(HttpError::Malformed("line too long"));
    }
    Ok(Some((
        unread.get(..end).unwrap_or_default(),
        end.saturating_add(2),
    )))
}

enum Body {
    Length(u64),
    Chunked,
    UntilClose,
}

struct Framing {
    body: Body,
    keep_alive: bool,
}

impl Framing {
    fn from_headers(headers: &[httparse::Header<'_>], http10: bool) -> Result<Self, HttpError> {
        let mut length: Option<u64> = None;
        let mut chunked = false;
        let mut transfer_encoded = false;
        let mut keep_alive = !http10;
        for header in headers {
            let value = std::str::from_utf8(header.value)
                .map_err(|_| HttpError::Malformed("header value"))?
                .trim();
            if header.name.eq_ignore_ascii_case("content-length") {
                // Digits only: `u64::from_str` would also take "+5".
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(HttpError::Malformed("content-length"));
                }
                let parsed: u64 = value
                    .parse()
                    .map_err(|_| HttpError::Malformed("content-length"))?;
                if length.is_some_and(|length| length != parsed) {
                    return Err(HttpError::Malformed("conflicting content-length"));
                }
                length = Some(parsed);
            } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
                transfer_encoded = true;
                chunked = value
                    .rsplit(',')
                    .next()
                    .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"));
            } else if header.name.eq_ignore_ascii_case("connection") {
                for option in value.split(',').map(str::trim) {
                    if option.eq_ignore_ascii_case("close") {
                        keep_alive = false;
                    } else if option.eq_ignore_ascii_case("keep-alive") {
                        keep_alive = true;
                    }
                }
            }
        }
        // RFC 9112 6.3: Transfer-Encoding overrides Content-Length; a final
        // coding other than chunked is read until close; a response with
        // both cannot be trusted to leave the connection in sync.
        // HTTP/1.0 has no Transfer-Encoding (RFC 9112 6.1): faulty framing.
        if transfer_encoded && (length.is_some() || !chunked || http10) {
            keep_alive = false;
        }
        let body = if chunked {
            Body::Chunked
        } else if transfer_encoded {
            Body::UntilClose
        } else if let Some(length) = length {
            Body::Length(length)
        } else {
            Body::UntilClose
        };
        Ok(Self { body, keep_alive })
    }
}

fn parse_chunk_size(line: &[u8]) -> Result<u64, HttpError> {
    let size = line.split(|byte| *byte == b';').next().unwrap_or_default();
    let size = std::str::from_utf8(size)
        .map_err(|_| HttpError::Malformed("chunk size"))?
        .trim();
    if size.is_empty() || size.len() > 16 || !size.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HttpError::Malformed("chunk size"));
    }
    u64::from_str_radix(size, 16).map_err(|_| HttpError::Malformed("chunk size"))
}

/// How many of `bytes` a body with `limit` bytes left takes.
fn take_up_to(bytes: &[u8], limit: u64) -> usize {
    usize::try_from(limit).map_or(bytes.len(), |limit| limit.min(bytes.len()))
}

#[cfg(test)]
#[path = "http1_tests.rs"]
mod tests;
