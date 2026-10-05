use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::*;

/// A transmit queue with `budget` bytes of space left (the test refills it,
/// as a drained socket would), recording what was queued, and a peer that
/// replays `response` in `read_chunk`-sized reads, then EOF when `close`.
struct MemoryIo {
    budget: usize,
    sent: Vec<u8>,
    response: VecDeque<u8>,
    read_chunk: usize,
    close: bool,
    fail_writes: bool,
    /// The peer answers once this many request bytes arrived.
    respond_after: usize,
}

impl MemoryIo {
    fn new(budget: usize, response: &[u8]) -> Self {
        Self {
            budget,
            sent: Vec::new(),
            response: response.iter().copied().collect(),
            read_chunk: 7,
            close: false,
            fail_writes: false,
            respond_after: 0,
        }
    }
}

impl UploadIo for MemoryIo {
    fn write_message(&mut self, parts: &[TxPart<'_>]) -> io::Result<usize> {
        if self.fail_writes {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "reset"));
        }
        let mut count = 0;
        for part in parts {
            let slice: &[u8] = match part {
                TxPart::Copy(bytes) => bytes,
                TxPart::Share(bytes) => bytes,
            };
            let take = slice.len().min(self.budget);
            self.sent.extend_from_slice(&slice[..take]);
            self.budget -= take;
            count += take;
        }
        if count == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(count)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.sent.len() < self.respond_after {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if self.response.is_empty() {
            return if self.close {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let count = self.read_chunk.min(buf.len()).min(self.response.len());
        for slot in &mut buf[..count] {
            *slot = self.response.pop_front().unwrap();
        }
        Ok(count)
    }
}

fn segment(file_name: &str, body: &'static [u8]) -> UploadRequest {
    UploadRequest {
        target: UploadTarget::Segment { sequence: 0 },
        file_name: file_name.to_string(),
        content_type: "video/mp2t",
        body: Bytes::from_static(body),
        fresh_connection: false,
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn targets_follow_the_youtube_and_path_url_shapes() {
    let youtube = HlsPutTarget::parse(
        "https://a.upload.youtube.com/http_upload_hls?cid=key&copy=0&file=out.m3u8",
    )
    .unwrap();
    assert!(youtube.tls);
    assert_eq!(
        (youtube.host.as_str(), youtube.port),
        ("a.upload.youtube.com", 443)
    );
    assert_eq!(
        youtube.request_target(&segment("r1-7.ts", b"")),
        "/http_upload_hls?cid=key&copy=0&file=r1-7.ts"
    );

    let origin = HlsPutTarget::parse("http://[::1]:8080/live/out.m3u8").unwrap();
    assert_eq!(
        (origin.host.as_str(), origin.port, origin.tls),
        ("::1", 8080, false)
    );
    assert_eq!(origin.host_header, "[::1]:8080");
    assert_eq!(
        origin.request_target(&segment("r1-7.ts", b"")),
        "/live/r1-7.ts"
    );

    assert!(HlsPutTarget::parse("rtmp://host/app/key").is_none());
}

#[test]
fn an_upload_sends_head_then_body_and_reads_the_status() {
    let target = HlsPutTarget::parse("http://origin:8080/live/out.m3u8").unwrap();
    let request = segment("r1-0.ts", b"0123456789");
    let mut exchange = Exchange::new(&target, &request, deadline());
    // Four bytes of transmit space per write, a response in 7-byte reads.
    let mut io = MemoryIo::new(4, b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
    io.respond_after = exchange.unsent();
    let mut scratch = [0u8; 64];
    let mut steps = 0;
    let response = loop {
        steps += 1;
        assert!(steps < 200, "no progress");
        io.budget = 4;
        match exchange.advance(&mut io, &mut scratch) {
            ExchangeStep::Done(response) => break response,
            ExchangeStep::Blocked => {}
            ExchangeStep::Failed(error) => panic!("{error}"),
        }
    };
    assert_eq!(response.status, 202);
    assert!(response.keep_alive);
    assert_eq!(
        io.sent,
        [
            b"PUT /live/r1-0.ts HTTP/1.1\r\nHost: origin:8080\r\nUser-Agent: ".as_slice(),
            HLS_PUT_USER_AGENT.as_bytes(),
            b"\r\nContent-Type: video/mp2t\r\nContent-Length: 10\r\n\r\n0123456789",
        ]
        .concat()
    );
    assert_eq!(exchange.unsent(), 0);
}

#[test]
fn an_early_rejection_completes_before_the_body_is_sent() {
    let target = HlsPutTarget::parse("http://origin/live/out.m3u8").unwrap();
    let request = segment("r1-0.ts", b"a large body the server will not wait for");
    let mut exchange = Exchange::new(&target, &request, deadline());
    let mut io = MemoryIo::new(
        16,
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    io.read_chunk = 256;
    let step = exchange.advance(&mut io, &mut [0u8; 256]);
    assert_eq!(
        step,
        ExchangeStep::Done(Response {
            status: 401,
            keep_alive: false
        })
    );
    assert!(exchange.unsent() > 0);
}

#[test]
fn a_reset_or_a_close_mid_response_fails_the_exchange() {
    let target = HlsPutTarget::parse("http://origin/live/out.m3u8").unwrap();
    let request = segment("r1-0.ts", b"body");

    let mut reset = MemoryIo::new(1024, b"");
    reset.fail_writes = true;
    let mut exchange = Exchange::new(&target, &request, deadline());
    assert!(matches!(
        exchange.advance(&mut reset, &mut [0u8; 64]),
        ExchangeStep::Failed(_)
    ));

    let mut closed = MemoryIo::new(1024, b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc");
    closed.close = true;
    let mut exchange = Exchange::new(&target, &request, deadline());
    let step = loop {
        match exchange.advance(&mut closed, &mut [0u8; 64]) {
            ExchangeStep::Blocked => continue,
            other => break other,
        }
    };
    assert!(matches!(step, ExchangeStep::Failed(error) if error.contains("closed")));
}
