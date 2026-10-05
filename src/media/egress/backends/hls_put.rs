//! One HLS PUT output's HTTP work on an egress shard: the target it uploads
//! to, and the request/response exchange over a connected transport. What to
//! upload is `media::hls::upload_policy`; connecting is the shard's
//! (`hls_put_shard.rs`).
//!
//! The request head is copied into the transmit queue; the segment body is
//! queued as a shared `Bytes` slice, so payload is never copied in userspace
//! (kTLS encrypts in the kernel for HTTPS).
use std::io::{self, Read};
use std::time::Instant;

use bytes::Bytes;
use reqwest::Url;

use super::compio_tcp::TxPart;
use crate::media::egress::http1::{
    HttpError, RequestHead, Response, ResponseReader, write_request_head,
};
use crate::media::egress::tls::TlsTcpConnection;
use crate::media::hls::upload_policy::{UploadRequest, UploadTarget};

/// `<manufacturer> / <model> / <version>`, as YouTube asks and Akamai
/// requires.
pub(crate) const HLS_PUT_USER_AGENT: &str =
    concat!("Restream / restream / ", env!("CARGO_PKG_VERSION"));

/// Where an HLS PUT output uploads.
#[derive(Debug, Clone)]
pub(crate) struct HlsPutTarget {
    playlist_url: Url,
    /// Name to resolve (IPv6 without brackets).
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: bool,
    /// `Host` header value: the port only when it is not the default.
    host_header: String,
}

impl HlsPutTarget {
    pub(crate) fn parse(url: &str) -> Option<Self> {
        let playlist_url = Url::parse(url).ok()?;
        let tls = match playlist_url.scheme() {
            "http" => false,
            "https" => true,
            _ => return None,
        };
        let host_str = playlist_url.host_str()?.to_string();
        let port = playlist_url.port_or_known_default()?;
        let host_header = match playlist_url.port() {
            Some(port) => format!("{host_str}:{port}"),
            None => host_str.clone(),
        };
        Some(Self {
            host: host_str.trim_matches(['[', ']']).to_string(),
            port,
            tls,
            host_header,
            playlist_url,
        })
    }

    /// Origin-form request target (path and query) for `request`.
    pub(crate) fn request_target(&self, request: &UploadRequest) -> String {
        let url = match request.target {
            UploadTarget::Playlist { .. } => self.playlist_url.clone(),
            UploadTarget::Segment { .. } => crate::media::hls::upload::derive_hls_upload_url(
                &self.playlist_url,
                &request.file_name,
            ),
        };
        let mut target = url.path().to_string();
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        target
    }
}

/// The byte transport an exchange runs over: a shard connection in
/// production, an in-memory peer in tests.
pub(crate) trait UploadIo {
    fn write_shared(&mut self, parts: &[TxPart<'_>]) -> io::Result<usize>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

impl UploadIo for TlsTcpConnection {
    fn write_shared(&mut self, parts: &[TxPart<'_>]) -> io::Result<usize> {
        TlsTcpConnection::write_shared(self, parts)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(self, buf)
    }
}

/// One request in flight on a connection.
pub(crate) struct Exchange {
    head: Vec<u8>,
    body: Bytes,
    written: usize,
    reader: ResponseReader,
    /// The response must be complete by then (the request timeout).
    pub(crate) deadline: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExchangeStep {
    Done(Response),
    /// Waiting for transmit space or response bytes.
    Blocked,
    Failed(String),
}

impl Exchange {
    pub(crate) fn new(target: &HlsPutTarget, request: &UploadRequest, deadline: Instant) -> Self {
        let mut head = Vec::with_capacity(256);
        write_request_head(
            &RequestHead {
                method: "PUT",
                target: &target.request_target(request),
                host: &target.host_header,
                user_agent: HLS_PUT_USER_AGENT,
                content_type: request.content_type,
                content_length: request.body.len(),
            },
            &mut head,
        );
        Self {
            head,
            body: request.body.clone(),
            written: 0,
            reader: ResponseReader::new(),
            deadline,
        }
    }

    fn total(&self) -> usize {
        self.head.len().saturating_add(self.body.len())
    }

    /// Queue what fits, then read what arrived. A server may answer before
    /// the whole body is sent (a 401 for an expired key, say); that answer
    /// completes the exchange.
    pub(crate) fn advance(&mut self, io: &mut impl UploadIo, scratch: &mut [u8]) -> ExchangeStep {
        while self.written < self.total() {
            let result = if let Some(head) = self.head.get(self.written..).filter(|h| !h.is_empty())
            {
                io.write_shared(&[TxPart::Copy(head), TxPart::Share(self.body.clone())])
            } else {
                let offset = self.written.saturating_sub(self.head.len());
                io.write_shared(&[TxPart::Share(self.body.slice(offset..))])
            };
            match result {
                Ok(0) => break,
                Ok(count) => self.written = self.written.saturating_add(count),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return ExchangeStep::Failed(format!("send: {error}")),
            }
        }
        loop {
            match io.read(scratch) {
                Ok(0) => {
                    return match self.reader.finish() {
                        Ok(response) => ExchangeStep::Done(self.reusable(response)),
                        Err(error) => ExchangeStep::Failed(error.to_string()),
                    };
                }
                Ok(count) => match self.reader.feed(scratch.get(..count).unwrap_or_default()) {
                    Ok(Some(response)) => return ExchangeStep::Done(self.reusable(response)),
                    Ok(None) => {}
                    Err(error) => return ExchangeStep::Failed(http_error(error)),
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return ExchangeStep::Blocked;
                }
                Err(error) => return ExchangeStep::Failed(format!("receive: {error}")),
            }
        }
    }

    /// An answer before the whole request was sent, or with bytes after
    /// it, leaves the connection out of step: the next request on it would
    /// be read as the rest of this one. Such a connection is not reused.
    fn reusable(&self, mut response: Response) -> Response {
        if self.written < self.total() || self.reader.has_unread() {
            response.keep_alive = false;
        }
        response
    }

    /// Request bytes not yet queued.
    #[cfg(test)]
    pub(crate) fn unsent(&self) -> usize {
        self.total().saturating_sub(self.written)
    }
}

fn http_error(error: HttpError) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "hls_put_tests.rs"]
mod tests;
