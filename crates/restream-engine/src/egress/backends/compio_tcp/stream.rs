use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IoSlice, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::rc::Rc;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};

use super::super::tcp::TcpReadyLeaf;

/// Bounded protocol-facing buffers. All socket reads and writes are driven by
/// Compio completions; the synchronous protocol engine only consumes or
/// produces bytes here.
pub(super) const TRANSPORT_BUFFER_CAPACITY: usize = 4096;
/// Default TX staging bound per connection (`IoBuffers::transmit_capacity`).
/// Each transmit completion carries up to this many bytes, so it sets the
/// completion (event + visit) rate per byte of fan-out; RX stays at
/// `TRANSPORT_BUFFER_CAPACITY`.
pub(super) const TRANSMIT_BUFFER_CAPACITY: usize = 64 * 1024;
pub(crate) type SharedIoBuffers = std::rc::Rc<std::cell::RefCell<IoBuffers>>;

#[derive(Debug, Default)]
pub(crate) struct IoBuffers {
    pub(super) received: VecDeque<u8>,
    /// TX staging, in wire order: shared payload slices and sealed runs of
    /// copied bytes. The transmit worker swaps the whole list out and sends it
    /// with one vectored write; `pending_write_bytes` (staged plus in flight)
    /// keeps it within `TRANSMIT_BUFFER_CAPACITY`.
    outgoing: Vec<Bytes>,
    /// Open run of copied bytes (chunk headers, control messages, TLS
    /// records), sealed into `outgoing` before the next shared slice.
    copied: BytesMut,
    pub(super) record_type: Option<(usize, u8)>,
    pending_write_bytes: usize,
    /// TX staging bound (`TRANSMIT_BUFFER_CAPACITY` unless the connection's
    /// owner raised it with `set_transmit_capacity`).
    transmit_capacity: usize,
    /// The staged bytes stop inside a message whose rest is still to come
    /// (`write_shared_message` could not stage all of it): the transmit
    /// worker sends them with `MSG_MORE`, so the kernel coalesces them with
    /// the rest instead of pushing a short segment (or, under kTLS, every
    /// 16 KiB record as its own segment).
    message_open: bool,
    rx_space_waker: Option<std::task::Waker>,
    tx_waker: Option<std::task::Waker>,
    rx_resume_waker: Option<std::task::Waker>,
    receive_armed: bool,
    ancillary_mode: bool,
    pub(super) ktls_active: bool,
    eof: bool,
    error: Option<(io::ErrorKind, String)>,
}

/// One piece of a zero-copy write: bytes the caller owns only for the call
/// (copied), or a payload slice whose buffer is shared into the send.
pub(crate) enum TxPart<'a> {
    Copy(&'a [u8]),
    Share(Bytes),
}

/// Shared slices shorter than this are copied instead: one more iovec costs
/// more than copying a few hundred bytes.
const SHARE_MIN_BYTES: usize = 512;

/// Initial capacity of the copied-bytes run (headers and control messages
/// between shared payload slices).
const COPIED_RUN_CAPACITY: usize = 4096;

/// Kernel limit on iovecs per `writev` (`UIO_MAXIOV`).
const MAX_WRITE_SEGMENTS: usize = 1024;

/// Staging stops short of this many segments in one batch: a shared slice
/// adds at most two (a sealed copied run and the slice), and a batch over
/// `UIO_MAXIOV` would turn every send into EINVAL.
const MAX_BATCH_SEGMENTS: usize = MAX_WRITE_SEGMENTS - 2;

impl TxPart<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Copy(bytes) => bytes,
            Self::Share(bytes) => bytes,
        }
    }
}

impl IoBuffers {
    fn stage_copy(&mut self, bytes: &[u8]) {
        self.copied.extend_from_slice(bytes);
    }

    fn stage_share(&mut self, bytes: Bytes) {
        if bytes.len() < SHARE_MIN_BYTES {
            self.copied.extend_from_slice(&bytes);
            return;
        }
        self.seal_copied();
        self.outgoing.push(bytes);
    }

    fn seal_copied(&mut self) {
        if !self.copied.is_empty() {
            let run = self.copied.split().freeze();
            self.outgoing.push(run);
        }
    }

    /// Stage a prefix of `parts` within the TX bound and the batch segment
    /// limit; `WouldBlock` when nothing fits. `message`: the parts are the
    /// rest of one message, so a prefix leaves it open (`message_open`).
    fn stage_parts(&mut self, parts: &[TxPart<'_>], message: bool) -> io::Result<usize> {
        self.check_transmit()?;
        let mut available = self
            .transmit_capacity
            .saturating_sub(self.pending_write_bytes);
        let mut count = 0;
        for part in parts {
            let len = part.as_slice().len();
            let take = available.min(len);
            if self.outgoing.len() >= MAX_BATCH_SEGMENTS {
                break;
            }
            if take == 0 {
                if len == 0 {
                    continue;
                }
                break;
            }
            match part {
                TxPart::Copy(bytes) => self.stage_copy(&bytes[..take]),
                TxPart::Share(bytes) => self.stage_share(bytes.slice(..take)),
            }
            self.pending_write_bytes += take;
            available -= take;
            count += take;
        }
        let total: usize = parts.iter().map(|part| part.as_slice().len()).sum();
        if count == 0 && total > 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.message_open = message && count < total;
        wake(&mut self.tx_waker);
        Ok(count)
    }

    /// Stage a prefix of `buf` as copied bytes within the TX bound;
    /// `WouldBlock` when nothing fits.
    fn write_copied(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check_transmit()?;
        let available = self
            .transmit_capacity
            .saturating_sub(self.pending_write_bytes);
        let count = buf.len().min(available);
        if count == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.stage_copy(&buf[..count]);
        self.pending_write_bytes += count;
        wake(&mut self.tx_waker);
        Ok(count)
    }

    /// Copies only (the copied run adds no segment), so it needs no
    /// `MAX_BATCH_SEGMENTS` guard; a zero-copy variant must use `stage_parts`.
    fn write_copied_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.check_transmit()?;
        let mut available = self
            .transmit_capacity
            .saturating_sub(self.pending_write_bytes);
        let mut count = 0;
        for buf in bufs.iter().filter(|buf| !buf.is_empty()) {
            let take = available.min(buf.len());
            if take == 0 {
                break;
            }
            self.stage_copy(&buf[..take]);
            self.pending_write_bytes += take;
            available -= take;
            count += take;
        }
        if count == 0 && bufs.iter().any(|buf| !buf.is_empty()) {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            wake(&mut self.tx_waker);
            Ok(count)
        }
    }

    /// A failed connection refuses further writes with its stored error.
    fn check_transmit(&self) -> io::Result<()> {
        match &self.error {
            Some((kind, error)) => Err(io::Error::new(*kind, error.clone())),
            None => Ok(()),
        }
    }

    /// Raise (or lower) the TX staging bound; at least one byte, so a
    /// write can always make progress.
    fn limit_transmit(&mut self, bytes: usize) {
        self.transmit_capacity = bytes.max(1);
    }

    fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty() || !self.copied.is_empty()
    }

    /// Swap the staged segments into `output`; `Ready(true)` when they stop
    /// inside an open message (see `message_open`). Pending, with the waker
    /// kept, until something is staged.
    fn poll_take_transmit(&mut self, cx: &mut Context<'_>, output: &mut Vec<Bytes>) -> Poll<bool> {
        match self.take_staged(output) {
            Some(message_open) => Poll::Ready(message_open),
            None => {
                self.tx_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    /// Swap the staged segments into `output`: `None` when nothing is
    /// staged, else whether they stop inside an open message.
    fn take_staged(&mut self, output: &mut Vec<Bytes>) -> Option<bool> {
        if !self.has_outgoing() {
            return None;
        }
        // Hand the staged segments to the writer by swapping lists; both keep
        // their capacity, so payload is never copied and the list is not
        // reallocated. (Sealed copied runs and Compio's iovec array are small
        // per-flush allocations.)
        self.seal_copied();
        std::mem::swap(output, &mut self.outgoing);
        Some(self.message_open)
    }

    /// The transmit worker sent `count` staged bytes: their room in the TX
    /// bound frees up.
    fn complete_transmit(&mut self, count: usize) {
        self.pending_write_bytes = self.pending_write_bytes.saturating_sub(count);
    }

    /// Keep a socket error for the protocol's next read or write.
    fn fail(&mut self, error: &io::Error) {
        self.error = Some((error.kind(), error.to_string()));
    }

    fn pending_receive_bytes(&self) -> usize {
        self.received.len()
    }

    /// See `CompioTcpStream::complete_tls_records_len`.
    fn complete_tls_records_len(&self) -> usize {
        let received = &self.received;
        whole_tls_records_len(received.len(), |at| received.get(at).copied())
    }

    /// See `CompioTcpStream::receive_closed`.
    fn receive_closed(&self) -> bool {
        self.eof || self.error.is_some()
    }

    /// See `CompioTcpStream::has_buffered_receive`.
    fn has_buffered_receive(&self) -> bool {
        !self.received.is_empty() || self.receive_closed()
    }

    /// Let the receive worker post its next receive.
    fn resume_receive(&mut self) {
        self.receive_armed = true;
        wake(&mut self.rx_resume_waker);
    }

    /// TLS before kTLS: receives go through the ancillary worker, which
    /// reads synchronously until the hand-off and then with `recvmsg`.
    fn start_ancillary_receive(&mut self) {
        debug_assert!(!self.ktls_active, "ancillary receive starts before kTLS");
        self.ancillary_mode = true;
    }

    /// The kernel took over TLS. Only the ancillary worker reads record
    /// types, and every staged byte went to rustls first, so no record is
    /// split between rustls and kernel TLS.
    fn enter_ktls(&mut self) {
        debug_assert!(
            self.ancillary_mode,
            "kTLS receive needs the ancillary worker"
        );
        debug_assert!(
            self.received.is_empty(),
            "staged receive bytes at the kTLS hand-off"
        );
        self.ktls_active = true;
    }

    /// Move the front of the receive buffer into `buf` and wake a receive
    /// waiting for room. Nothing buffered: the stored error (once), EOF as
    /// `Ok(0)`, else `WouldBlock`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = buf.len().min(self.received.len());
        self.drain_received(&mut buf[..count])
    }

    /// One read with its TLS record type: 23 (application data) before
    /// kTLS; after it, at most the rest of the current kTLS record.
    fn read_record(&mut self, buf: &mut [u8]) -> io::Result<(usize, u8)> {
        if !self.ktls_active {
            return self.read(buf).map(|count| (count, 23));
        }
        let received_len = self.received.len();
        let Some((remaining, record_type)) = self.record_type.as_mut() else {
            if received_len == 0 {
                return self.closed_read().map(|()| (0, 23));
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "kTLS receive data missing its record type",
            ));
        };
        let count = buf.len().min(received_len).min(*remaining);
        *remaining -= count;
        let record_type = *record_type;
        if *remaining == 0 {
            self.record_type = None;
        }
        self.drain_received(&mut buf[..count]).map(|count| {
            if count > 0 {
                (count, record_type)
            } else {
                (0, 23)
            }
        })
    }

    fn drain_received(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        take_front(&mut self.received, dst);
        if dst.is_empty() {
            return self.closed_read().map(|()| 0);
        }
        wake(&mut self.rx_space_waker);
        Ok(dst.len())
    }

    /// A read found nothing buffered: the stored error (reported once),
    /// `Ok` at EOF, else `WouldBlock`.
    fn closed_read(&mut self) -> io::Result<()> {
        if let Some((kind, message)) = self.error.take() {
            Err(io::Error::new(kind, message))
        } else if self.eof {
            Ok(())
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    /// Room for the next receive, at most `IO_CHUNK`; Pending, with the
    /// waker kept, while the buffer is full or a kTLS record is still
    /// being read.
    fn poll_receive_room(&mut self, cx: &mut Context<'_>) -> Poll<usize> {
        let room = TRANSPORT_BUFFER_CAPACITY.saturating_sub(self.received.len());
        let waiting_for_ktls_record = self.ktls_active && self.record_type.is_some();
        if room > 0 && !waiting_for_ktls_record {
            Poll::Ready(room.min(IO_CHUNK))
        } else {
            self.rx_space_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// Consume the arm a protocol visit gave (`resume_receive`); Pending,
    /// with the waker kept, until there is one.
    fn poll_receive_arm(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.receive_armed {
            self.receive_armed = false;
            Poll::Ready(())
        } else {
            self.rx_resume_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// The record type of a `recvmsg` completion: from its control data
    /// once kTLS is active; before that, control data is an error.
    fn ktls_record_type(&self, control: &[u8], truncated: bool) -> io::Result<Option<u8>> {
        if self.ktls_active {
            crate::egress::tls::ktls::record_type_from_control(control, truncated).map(Some)
        } else if !control.is_empty() || truncated {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected RTMPS ancillary control data",
            ))
        } else {
            Ok(None)
        }
    }

    /// Stage one receive attempt for the next protocol visit. `None`: the
    /// pre-kTLS readiness poll read nothing (or kTLS was installed
    /// meanwhile), so the receive re-arms itself and retries without
    /// waiting for a visit. Bytes extend the buffer, tagged with their kTLS
    /// record type when given; zero bytes are EOF; an error is kept for the
    /// protocol's next read.
    fn stage_receive(
        &mut self,
        attempt: Option<io::Result<&[u8]>>,
        record_type: Option<u8>,
    ) -> ReceiveStep {
        match attempt {
            None => {
                self.receive_armed = true;
                ReceiveStep::Retry
            }
            Some(Ok([])) => {
                self.eof = true;
                ReceiveStep::Stop
            }
            Some(Ok(bytes)) => {
                self.received.extend(bytes);
                debug_assert!(self.received.len() <= TRANSPORT_BUFFER_CAPACITY);
                if let Some(record_type) = record_type {
                    self.record_type = Some((bytes.len(), record_type));
                }
                ReceiveStep::Continue
            }
            Some(Err(error)) => {
                self.fail(&error);
                ReceiveStep::Stop
            }
        }
    }

    /// Test transport: see `CompioTcpStream::push_received_for_test`.
    #[cfg(test)]
    fn push_received_for_test(&mut self, bytes: &[u8]) {
        self.received.extend(bytes);
    }

    pub(super) fn new() -> SharedIoBuffers {
        std::rc::Rc::new(std::cell::RefCell::new(Self {
            receive_armed: true,
            transmit_capacity: TRANSMIT_BUFFER_CAPACITY,
            // A non-zero original capacity lets `split` + `reserve` reuse or
            // regrow the run in one step instead of doubling up from zero.
            copied: BytesMut::with_capacity(COPIED_RUN_CAPACITY),
            ..Self::default()
        }))
    }
}

/// A Compio-owned TCP descriptor plus a bounded adapter for the synchronous
/// RTMP/Rustls state machines. The adapter never performs socket syscalls.
#[derive(Debug)]
pub(crate) enum CompioTcpStream {
    Compio {
        stream: Rc<compio::net::TcpStream>,
        buffers: SharedIoBuffers,
    },
    #[cfg(test)]
    Std(TcpStream),
}

impl CompioTcpStream {
    #[cfg(test)]
    pub(crate) fn from_std(stream: TcpStream) -> Self {
        Self::Std(stream)
    }

    pub(crate) fn from_compio(stream: compio::net::TcpStream) -> Self {
        Self::Compio {
            stream: Rc::new(stream),
            buffers: IoBuffers::new(),
        }
    }

    pub(crate) fn io_stream(&self) -> Option<Rc<compio::net::TcpStream>> {
        match self {
            Self::Compio { stream, .. } => Some(stream.clone()),
            #[cfg(test)]
            Self::Std(_) => None,
        }
    }

    pub(crate) fn io_buffers(&self) -> Option<SharedIoBuffers> {
        match self {
            Self::Compio { buffers, .. } => Some(buffers.clone()),
            #[cfg(test)]
            Self::Std(_) => None,
        }
    }

    pub(crate) fn pending_write_bytes(&self) -> usize {
        self.io_buffers()
            .map_or(0, |buffers| buffers.borrow().pending_write_bytes)
    }
    pub(crate) fn pending_receive_bytes(&self) -> usize {
        self.io_buffers()
            .map_or(0, |buffers| buffers.borrow().pending_receive_bytes())
    }

    /// Length of the run of whole TLS records (5-byte header, then the
    /// length it gives) at the front of the receive buffer. Before the kTLS
    /// hand-off only whole records may go to rustls, so that no record is
    /// split between rustls and kernel TLS.
    pub(crate) fn complete_tls_records_len(&self) -> usize {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow().complete_tls_records_len(),
            #[cfg(test)]
            Self::Std(stream) => {
                let mut peeked = vec![0_u8; 64 * 1024];
                let count = stream.peek(&mut peeked).unwrap_or(0);
                whole_tls_records_len(count, |at| peeked.get(at).copied())
            }
        }
    }

    /// The receive side ended (EOF or a socket error); bytes received
    /// before it may still be buffered.
    pub(crate) fn receive_closed(&self) -> bool {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow().receive_closed(),
            // Test-only: detects a clean EOF with nothing pending; data
            // before a FIN reads as "not closed".
            #[cfg(test)]
            Self::Std(stream) => match stream.peek(&mut [0_u8; 1]) {
                Ok(count) => count == 0,
                Err(error) => error.kind() != io::ErrorKind::WouldBlock,
            },
        }
    }

    /// Test transport: the staged TX bytes, as the transmit worker would
    /// take them (they stay counted as pending until `drain_for_test`).
    #[cfg(test)]
    pub(crate) fn take_staged_for_test(&self) -> Vec<u8> {
        let buffers = self.io_buffers().expect("a Compio stream");
        let mut segments = Vec::new();
        let _ = buffers.borrow_mut().take_staged(&mut segments);
        segments
            .iter()
            .flat_map(|segment| segment.iter().copied())
            .collect()
    }

    /// Test transport: the transmit worker finished every staged write.
    #[cfg(test)]
    pub(crate) fn drain_for_test(&self) {
        let buffers = self.io_buffers().expect("a Compio stream");
        buffers.borrow_mut().complete_transmit(usize::MAX);
    }

    /// Test transport: the receive worker saw EOF.
    #[cfg(test)]
    pub(crate) fn end_receive_for_test(&self) {
        let buffers = self.io_buffers().expect("a Compio stream");
        buffers.borrow_mut().stage_receive(Some(Ok(&[])), None);
    }

    /// Test transport: bytes the receive worker read from the socket
    /// (unbounded, unlike the worker's reads).
    #[cfg(test)]
    pub(crate) fn push_received_for_test(&self, bytes: &[u8]) {
        let buffers = self.io_buffers().expect("a Compio stream");
        buffers.borrow_mut().push_received_for_test(bytes);
    }

    /// Whether the adapter holds receive state the protocol has not consumed
    /// yet (bytes, EOF, or an error). A receive completion is an edge event,
    /// so the scheduler must revisit a read-waiting leaf in this state itself.
    pub(crate) fn has_buffered_receive(&self) -> bool {
        self.io_buffers()
            .is_some_and(|buffers| buffers.borrow().has_buffered_receive())
    }

    pub(crate) fn resume_receive(&self) {
        if let Some(buffers) = self.io_buffers() {
            buffers.borrow_mut().resume_receive();
        }
    }
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Compio { stream, .. } => stream.as_fd(),
            #[cfg(test)]
            Self::Std(stream) => stream.as_fd(),
        }
    }
    /// TLS before kTLS: see `IoBuffers::start_ancillary_receive`.
    pub(crate) fn start_ancillary_receive(&self) {
        if let Some(buffers) = self.io_buffers() {
            buffers.borrow_mut().start_ancillary_receive();
        }
    }
    /// The kernel took over TLS: see `IoBuffers::enter_ktls`.
    pub(crate) fn enter_ktls(&self) {
        if let Some(buffers) = self.io_buffers() {
            buffers.borrow_mut().enter_ktls();
        }
    }

    pub(crate) fn read_record(&mut self, buf: &mut [u8]) -> io::Result<(usize, u8)> {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().read_record(buf),
            #[cfg(test)]
            Self::Std(stream) => crate::egress::tls::ktls::recv_record(stream.as_raw_fd(), buf),
        }
    }

    pub(crate) fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        let how = match how {
            std::net::Shutdown::Read => rustix::net::Shutdown::Read,
            std::net::Shutdown::Write => rustix::net::Shutdown::Write,
            std::net::Shutdown::Both => rustix::net::Shutdown::Both,
        };
        Ok(rustix::net::shutdown(self.as_fd(), how)?)
    }
    pub(crate) fn duplicate_std_fd(fd: BorrowedFd<'_>) -> io::Result<TcpStream> {
        let stream = TcpStream::from(rustix::io::fcntl_dupfd_cloexec(fd, 0)?);
        stream.set_nonblocking(true)?;
        Ok(stream)
    }
}

#[cfg(test)]
impl From<TcpStream> for CompioTcpStream {
    fn from(stream: TcpStream) -> Self {
        Self::from_std(stream)
    }
}

impl AsRawFd for CompioTcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.raw_fd()
    }
}

impl Read for CompioTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().read(buf),
            #[cfg(test)]
            Self::Std(stream) => stream.read(buf),
        }
    }
}

impl CompioTcpStream {
    /// Queue `parts` in wire order without copying shared payload slices.
    /// Accepts a prefix within the TX bound and reports its length, like
    /// `write_vectored`; `WouldBlock` when nothing fits.
    pub(crate) fn write_shared(&mut self, parts: &[TxPart<'_>]) -> io::Result<usize> {
        self.stage_shared(parts, false)
    }

    /// `write_shared` for `parts` that are the rest of one message (an HTTP
    /// request): until its last byte is staged, sends carry `MSG_MORE`, so
    /// the staging bound does not cut the message into short pushes.
    pub(crate) fn write_shared_message(&mut self, parts: &[TxPart<'_>]) -> io::Result<usize> {
        self.stage_shared(parts, true)
    }

    /// Raise (or lower) this connection's TX staging bound. A larger bound
    /// means fewer, larger sends: fewer completions, events and owner
    /// visits per byte. Staged shared slices are references, so the bound
    /// costs no copy; it only lets more of a payload be queued at once.
    pub(crate) fn set_transmit_capacity(&mut self, bytes: usize) {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().limit_transmit(bytes),
            #[cfg(test)]
            Self::Std(_) => {}
        }
    }

    fn stage_shared(&mut self, parts: &[TxPart<'_>], message: bool) -> io::Result<usize> {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().stage_parts(parts, message),
            #[cfg(test)]
            Self::Std(stream) => {
                let slices: Vec<IoSlice<'_>> = parts
                    .iter()
                    .map(|part| IoSlice::new(part.as_slice()))
                    .collect();
                stream.write_vectored(&slices)
            }
        }
    }
}

impl Write for CompioTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().write_copied(buf),
            #[cfg(test)]
            Self::Std(stream) => stream.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Compio { buffers, .. } => buffers.borrow_mut().write_copied_vectored(bufs),
            #[cfg(test)]
            Self::Std(stream) => stream.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Compio { .. } => Ok(()),
            #[cfg(test)]
            Self::Std(stream) => stream.flush(),
        }
    }
}
const IO_CHUNK: usize = 4096;

/// Move the front `dst.len()` bytes of `deque` into `dst`: at most two slice
/// copies (a ring's contents are at most two contiguous runs), then an O(1)
/// front drop. Per-byte iteration here dominated RTMP egress CPU.
/// Length of the run of whole TLS records (5-byte header, then the length
/// it gives) at the front of `len` bytes read through `byte_at`.
fn whole_tls_records_len(len: usize, byte_at: impl Fn(usize) -> Option<u8>) -> usize {
    let mut whole = 0;
    while let (Some(high), Some(low)) = (byte_at(whole + 3), byte_at(whole + 4)) {
        let end = whole + 5 + usize::from(u16::from_be_bytes([high, low]));
        if end > len {
            break;
        }
        whole = end;
    }
    whole
}

pub(super) fn take_front(deque: &mut VecDeque<u8>, dst: &mut [u8]) {
    let count = dst.len();
    let (front, back) = deque.as_slices();
    let first = front.len().min(count);
    dst[..first].copy_from_slice(&front[..first]);
    dst[first..].copy_from_slice(&back[..count - first]);
    deque.drain(..count);
}

fn wake(slot: &mut Option<std::task::Waker>) {
    if let Some(waker) = slot.take() {
        waker.wake();
    }
}

struct ReceiveRoom(SharedIoBuffers);

impl Future for ReceiveRoom {
    type Output = usize;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.borrow_mut().poll_receive_room(cx)
    }
}

struct ReceiveArm(SharedIoBuffers);

impl Future for ReceiveArm {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.borrow_mut().poll_receive_arm(cx)
    }
}

/// Swap the staged segments into `output`; true when they stop inside an
/// open message (see `IoBuffers::message_open`).
async fn take_transmit(buffers: &SharedIoBuffers, output: &mut Vec<Bytes>) -> bool {
    output.clear();
    std::future::poll_fn(|cx| buffers.borrow_mut().poll_take_transmit(cx, output)).await
}

async fn notify_readable(events: &flume::Sender<TcpReadyLeaf>, event: TcpReadyLeaf) -> bool {
    // The protocol adapter queues bounded writes; only its completion worker
    // waits for socket writability. A receive completion can therefore also
    // advance a protocol response without waiting for a kernel write event.
    events
        .send_async(TcpReadyLeaf {
            readable: true,
            writable: true,
            ..event
        })
        .await
        .is_ok()
}

/// Wake the protocol for a staged receive; a closed event channel stops the
/// worker.
async fn notify_staged(
    events: &flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
    step: ReceiveStep,
) -> ReceiveStep {
    if step == ReceiveStep::Retry {
        return step;
    }
    if notify_readable(events, event).await {
        step
    } else {
        ReceiveStep::Stop
    }
}

pub(super) async fn receive_worker(
    stream: Rc<compio::net::TcpStream>,
    buffers: SharedIoBuffers,
    events: flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
) {
    if buffers.borrow().ancillary_mode {
        receive_ancillary_worker(stream, buffers, events, event).await;
        return;
    }
    let mut stream = stream.as_ref();

    use compio::io::AsyncRead;

    let mut buffer = Vec::with_capacity(IO_CHUNK);
    loop {
        let room = ReceiveRoom(buffers.clone()).await;
        ReceiveArm(buffers.clone()).await;
        if buffer.capacity() != room {
            buffer = Vec::with_capacity(room);
        }
        let compio::BufResult(result, returned) = (&mut stream).read(buffer).await;
        buffer = returned;
        if let Ok(count) = result {
            debug_assert!(count <= room);
        }
        let step = buffers
            .borrow_mut()
            .stage_receive(Some(result.map(|count| &buffer[..count])), None);
        buffer.clear();
        if notify_staged(&events, event, step).await == ReceiveStep::Stop {
            return;
        }
    }
}

/// Keeps the Compio stream alive for an in-flight readiness poll.
struct PollTarget(Rc<compio::net::TcpStream>);

impl std::os::fd::AsFd for PollTarget {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// One Rustls-phase receive: wait for readability with an io_uring poll, which
/// consumes no bytes, then read synchronously on this owner thread. Returns
/// `None` when the socket was not readable after all, or when kTLS was
/// installed while the poll was pending.
async fn receive_before_ktls(
    stream: &Rc<compio::net::TcpStream>,
    buffers: &SharedIoBuffers,
    room: usize,
    data: &mut Vec<u8>,
) -> Option<io::Result<usize>> {
    let poll = compio::driver::op::PollOnce::new(
        PollTarget(Rc::clone(stream)),
        compio::driver::op::Interest::Readable,
    );
    let compio::BufResult(result, _) = compio::runtime::submit(poll).await;
    if let Err(error) = result {
        return Some(Err(error));
    }
    if buffers.borrow().ktls_active {
        return None;
    }
    data.clear();
    data.resize(room, 0);
    match rustix::net::recv(&**stream, &mut data[..], rustix::net::RecvFlags::DONTWAIT) {
        Ok((count, _)) => {
            data.truncate(count);
            Some(Ok(count))
        }
        Err(errno) => {
            data.clear();
            (errno != rustix::io::Errno::WOULDBLOCK).then(|| Err(errno.into()))
        }
    }
}

async fn receive_ancillary_worker(
    stream: Rc<compio::net::TcpStream>,
    buffers: SharedIoBuffers,
    events: flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
) {
    let owned_stream = Rc::clone(&stream);
    let mut stream = stream.as_ref();

    // Until kTLS is installed, receives never run in the kernel concurrently
    // with a protocol visit: an io_uring `recvmsg` in flight at the handoff
    // could consume post-handshake ciphertext Rustls never sees, leaving the
    // kernel's record sequence behind the extracted secrets. That phase waits
    // for readiness and reads synchronously, so every consumed byte is staged
    // before the next visit and the handoff's staged-receive check feeds it to
    // Rustls first. After the handoff, one-shot `recvmsg` completions carry the
    // kTLS record type.
    let mut data = Vec::with_capacity(IO_CHUNK);
    let mut control = vec![0; 24];
    loop {
        let room = ReceiveRoom(buffers.clone()).await;
        ReceiveArm(buffers.clone()).await;
        let step = if !buffers.borrow().ktls_active {
            receive_before_ktls_step(&owned_stream, &buffers, &events, event, room, &mut data).await
        } else {
            receive_ktls_step(
                &mut stream,
                &buffers,
                &events,
                event,
                room,
                &mut data,
                &mut control,
            )
            .await
        };
        if step == ReceiveStep::Stop {
            return;
        }
    }
}

/// What the receive loop does after one receive attempt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiveStep {
    /// Nothing was staged; receive again without a protocol visit.
    Retry,
    Continue,
    Stop,
}

/// One staged (pre-kTLS) receive round: wait for readiness, read synchronously,
/// and stage the bytes for the next protocol visit.
async fn receive_before_ktls_step(
    owned_stream: &Rc<compio::net::TcpStream>,
    buffers: &SharedIoBuffers,
    events: &flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
    room: usize,
    data: &mut Vec<u8>,
) -> ReceiveStep {
    let attempt = receive_before_ktls(owned_stream, buffers, room, data).await;
    if let Some(Ok(count)) = attempt {
        debug_assert!(count <= room);
    }
    let step = buffers.borrow_mut().stage_receive(
        attempt.map(|result| result.map(|count| &data[..count])),
        None,
    );
    data.clear();
    notify_staged(events, event, step).await
}

/// One kTLS receive round: one `recvmsg` completion with its record type.
async fn receive_ktls_step(
    stream: &mut &compio::net::TcpStream,
    buffers: &SharedIoBuffers,
    events: &flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
    room: usize,
    data: &mut Vec<u8>,
    control: &mut Vec<u8>,
) -> ReceiveStep {
    use compio::io::ancillary::{AsyncReadAncillary, ReturnFlags};
    if data.capacity() != room {
        *data = Vec::with_capacity(room);
    }
    let owned_data = std::mem::take(data);
    let owned_control = std::mem::take(control);
    let compio::BufResult(result, (returned_data, returned_control)) = (&mut *stream)
        .read_with_ancillary(owned_data, owned_control)
        .await;
    *data = returned_data;
    *control = returned_control;
    let step = {
        let mut state = buffers.borrow_mut();
        match result {
            Ok((0, _, _)) => state.stage_receive(Some(Ok(&[])), None),
            Ok((count, control_len, flags)) => {
                debug_assert!(count <= room);
                match state
                    .ktls_record_type(&control[..control_len], flags.contains(ReturnFlags::CTRUNC))
                {
                    Ok(record_type) => state.stage_receive(Some(Ok(&data[..count])), record_type),
                    Err(error) => state.stage_receive(Some(Err(error)), None),
                }
            }
            Err(error) => state.stage_receive(Some(Err(error)), None),
        }
    };
    data.clear();
    control.clear();
    control.resize(24, 0);
    notify_staged(events, event, step).await
}

/// Drop the first `count` written bytes: whole segments leave the list, the
/// first partially written one advances in place.
fn consume_segments(segments: &mut Vec<Bytes>, mut count: usize) {
    let mut written = 0;
    for segment in segments.iter_mut() {
        if count < segment.len() {
            segment.advance(count);
            break;
        }
        count -= segment.len();
        written += 1;
    }
    segments.drain(..written);
}

pub(super) async fn transmit_worker(
    stream: Rc<compio::net::TcpStream>,
    buffers: SharedIoBuffers,
    events: flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
) {
    use compio::buf::IntoInner;
    use compio::driver::op::{SendFlags, SendVectored};
    let mut data: Vec<Bytes> = Vec::new();
    loop {
        let flags = if take_transmit(&buffers, &mut data).await {
            SendFlags::NOSIGNAL | SendFlags::MORE
        } else {
            // What `AsyncWrite::write_vectored` on a Compio stream sends.
            SendFlags::NOSIGNAL
        };
        // A partial write keeps its unsent segments in `data` and writes them
        // next, ahead of anything queued later, without moving any bytes.
        while !data.is_empty() {
            let send = SendVectored::new(
                PollTarget(Rc::clone(&stream)),
                std::mem::take(&mut data),
                flags,
            );
            let compio::BufResult(result, returned) =
                compio::runtime::submit(send).await.into_inner();
            data = returned;
            let result = result.and_then(|count| {
                if count == 0 {
                    Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "RTMP socket write returned zero",
                    ))
                } else {
                    Ok(count)
                }
            });
            match result {
                Ok(count) => {
                    buffers.borrow_mut().complete_transmit(count);
                    consume_segments(&mut data, count);
                    if events
                        .send_async(TcpReadyLeaf {
                            writable: true,
                            ..event
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    buffers.borrow_mut().fail(&error);
                    let _ = events
                        .send_async(TcpReadyLeaf {
                            writable: true,
                            ..event
                        })
                        .await;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
