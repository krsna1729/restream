use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IoSlice, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::rc::Rc;
use std::task::{Context, Poll};

use super::super::tcp::TcpReadyLeaf;

/// Bounded protocol-facing buffers. All socket reads and writes are driven by
/// Compio completions; the synchronous protocol engine only consumes or
/// produces bytes here.
pub(super) const TRANSPORT_BUFFER_CAPACITY: usize = 4096;
pub(crate) type SharedIoBuffers = std::rc::Rc<std::cell::RefCell<IoBuffers>>;

#[derive(Debug, Default)]
pub(crate) struct IoBuffers {
    pub(super) received: VecDeque<u8>,
    outgoing: VecDeque<u8>,
    pub(super) record_type: Option<(usize, u8)>,
    pending_write_bytes: usize,
    rx_space_waker: Option<std::task::Waker>,
    tx_waker: Option<std::task::Waker>,
    rx_resume_waker: Option<std::task::Waker>,
    receive_armed: bool,
    ancillary_mode: bool,
    pub(super) ktls_active: bool,
    eof: bool,
    error: Option<(io::ErrorKind, String)>,
}

impl IoBuffers {
    pub(super) fn new() -> SharedIoBuffers {
        std::rc::Rc::new(std::cell::RefCell::new(Self {
            receive_armed: true,
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
            .map_or(0, |buffers| buffers.borrow().received.len())
    }

    /// Whether the adapter holds receive state the protocol has not consumed
    /// yet (bytes, EOF, or an error). A receive completion is an edge event,
    /// so the scheduler must revisit a read-waiting leaf in this state itself.
    pub(crate) fn has_buffered_receive(&self) -> bool {
        self.io_buffers().is_some_and(|buffers| {
            let buffers = buffers.borrow();
            !buffers.received.is_empty() || buffers.eof || buffers.error.is_some()
        })
    }

    pub(crate) fn resume_receive(&self) {
        if let Some(buffers) = self.io_buffers() {
            let mut buffers = buffers.borrow_mut();
            buffers.receive_armed = true;
            wake(&mut buffers.rx_resume_waker);
        }
    }
    pub(crate) fn raw_fd(&self) -> RawFd {
        match self {
            Self::Compio { stream, .. } => stream.as_raw_fd(),
            #[cfg(test)]
            Self::Std(stream) => stream.as_raw_fd(),
        }
    }
    pub(crate) fn set_ancillary_mode(&self) {
        if let Some(buffers) = self.io_buffers() {
            buffers.borrow_mut().ancillary_mode = true;
        }
    }
    pub(crate) fn set_ktls_mode(&self) {
        if let Some(buffers) = self.io_buffers() {
            buffers.borrow_mut().ktls_active = true;
        }
    }

    pub(crate) fn read_record(&mut self, buf: &mut [u8]) -> io::Result<(usize, u8)> {
        match self {
            Self::Compio { buffers, .. } => {
                let mut buffers = buffers.borrow_mut();
                let active = buffers.ktls_active;
                let received_len = buffers.received.len();
                let (record_type, count) = if active {
                    let Some((remaining, record_type)) = buffers.record_type.as_mut() else {
                        if received_len == 0 {
                            return if let Some((kind, message)) = buffers.error.take() {
                                Err(io::Error::new(kind, message))
                            } else if buffers.eof {
                                Ok((0, 23))
                            } else {
                                Err(io::ErrorKind::WouldBlock.into())
                            };
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "kTLS receive data missing its record type",
                        ));
                    };
                    let count = buf.len().min(received_len).min(*remaining);
                    *remaining -= count;
                    let record_type = *record_type;
                    let exhausted = *remaining == 0;
                    if exhausted {
                        buffers.record_type = None;
                    }
                    (record_type, count)
                } else {
                    (23, buf.len().min(buffers.received.len()))
                };
                for (dst, src) in buf[..count].iter_mut().zip(buffers.received.drain(..count)) {
                    *dst = src;
                }
                if count > 0 {
                    wake(&mut buffers.rx_space_waker);
                    Ok((count, record_type))
                } else if let Some((kind, message)) = buffers.error.take() {
                    Err(io::Error::new(kind, message))
                } else if buffers.eof {
                    Ok((0, 23))
                } else {
                    Err(io::ErrorKind::WouldBlock.into())
                }
            }
            #[cfg(test)]
            Self::Std(stream) => {
                super::super::rtmp_connection::rtmp_ktls::recv_record(stream.as_raw_fd(), buf)
            }
        }
    }

    pub(crate) fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        let how = match how {
            std::net::Shutdown::Read => libc::SHUT_RD,
            std::net::Shutdown::Write => libc::SHUT_WR,
            std::net::Shutdown::Both => libc::SHUT_RDWR,
        };
        let result = unsafe { libc::shutdown(self.raw_fd(), how) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    pub(crate) fn duplicate_std_fd(fd: RawFd) -> io::Result<TcpStream> {
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = unsafe { TcpStream::from_raw_fd(duplicate) };
        if let Err(error) = stream.set_nonblocking(true) {
            drop(stream);
            return Err(error);
        }
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
            Self::Compio { buffers, .. } => {
                let mut buffers = buffers.borrow_mut();
                let count = buf.len().min(buffers.received.len());
                for (dst, src) in buf[..count].iter_mut().zip(buffers.received.drain(..count)) {
                    *dst = src;
                }
                if count > 0 {
                    wake(&mut buffers.rx_space_waker);
                    Ok(count)
                } else if let Some((kind, message)) = buffers.error.take() {
                    Err(io::Error::new(kind, message))
                } else if buffers.eof {
                    Ok(0)
                } else {
                    Err(io::ErrorKind::WouldBlock.into())
                }
            }
            #[cfg(test)]
            Self::Std(stream) => stream.read(buf),
        }
    }
}

impl Write for CompioTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self {
            Self::Compio { buffers, .. } => {
                let mut buffers = buffers.borrow_mut();
                if let Some((kind, message)) = &buffers.error {
                    return Err(io::Error::new(*kind, message.clone()));
                }
                let available =
                    TRANSPORT_BUFFER_CAPACITY.saturating_sub(buffers.pending_write_bytes);
                let count = buf.len().min(available);
                if count == 0 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                buffers.outgoing.extend(&buf[..count]);
                buffers.pending_write_bytes += count;
                wake(&mut buffers.tx_waker);
                Ok(count)
            }
            #[cfg(test)]
            Self::Std(stream) => stream.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Compio { buffers, .. } => {
                let mut buffers = buffers.borrow_mut();
                if let Some((kind, message)) = &buffers.error {
                    return Err(io::Error::new(*kind, message.clone()));
                }
                let mut available =
                    TRANSPORT_BUFFER_CAPACITY.saturating_sub(buffers.pending_write_bytes);
                let mut count = 0;
                for buf in bufs.iter().filter(|buf| !buf.is_empty()) {
                    let take = available.min(buf.len());
                    if take == 0 {
                        break;
                    }
                    buffers.outgoing.extend(&buf[..take]);
                    buffers.pending_write_bytes += take;
                    available -= take;
                    count += take;
                }
                if count == 0 && bufs.iter().any(|buf| !buf.is_empty()) {
                    Err(io::ErrorKind::WouldBlock.into())
                } else {
                    wake(&mut buffers.tx_waker);
                    Ok(count)
                }
            }
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

fn wake(slot: &mut Option<std::task::Waker>) {
    if let Some(waker) = slot.take() {
        waker.wake();
    }
}

struct ReceiveRoom(SharedIoBuffers);

impl Future for ReceiveRoom {
    type Output = usize;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut buffers = self.0.borrow_mut();
        let room = TRANSPORT_BUFFER_CAPACITY.saturating_sub(buffers.received.len());
        let waiting_for_ktls_record = buffers.ktls_active && buffers.record_type.is_some();
        if room > 0 && !waiting_for_ktls_record {
            Poll::Ready(room.min(IO_CHUNK))
        } else {
            buffers.rx_space_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

struct ReceiveArm(SharedIoBuffers);

impl Future for ReceiveArm {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut buffers = self.0.borrow_mut();
        if buffers.receive_armed {
            buffers.receive_armed = false;
            Poll::Ready(())
        } else {
            buffers.rx_resume_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

async fn take_transmit(buffers: &SharedIoBuffers, output: &mut Vec<u8>) {
    output.clear();
    std::future::poll_fn(|cx| {
        let mut buffers = buffers.borrow_mut();
        if buffers.outgoing.is_empty() {
            buffers.tx_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = IO_CHUNK.min(buffers.outgoing.len());
        output.extend(buffers.outgoing.drain(..count));
        Poll::Ready(())
    })
    .await;
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

fn store_receive_error(buffers: &SharedIoBuffers, error: io::Error) {
    buffers.borrow_mut().error = Some((error.kind(), error.to_string()));
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
        match result {
            Ok(0) => {
                buffers.borrow_mut().eof = true;
                let _ = notify_readable(&events, event).await;
                return;
            }
            Ok(count) => {
                {
                    let mut state = buffers.borrow_mut();
                    debug_assert!(count <= room);
                    state.received.extend(buffer[..count].iter().copied());
                    debug_assert!(state.received.len() <= TRANSPORT_BUFFER_CAPACITY);
                }
                buffer.clear();
                if !notify_readable(&events, event).await {
                    return;
                }
            }
            Err(error) => {
                store_receive_error(&buffers, error);
                let _ = notify_readable(&events, event).await;
                return;
            }
        }
    }
}

/// Keeps the Compio stream alive for an in-flight readiness poll.
struct PollTarget(Rc<compio::net::TcpStream>);

impl std::os::fd::AsFd for PollTarget {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        // SAFETY: the `Rc` held by `self` keeps the descriptor open for the
        // lifetime of the borrow.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(self.0.as_raw_fd()) }
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
    let count = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            data.as_mut_ptr().cast(),
            room,
            libc::MSG_DONTWAIT,
        )
    };
    if count < 0 {
        let error = io::Error::last_os_error();
        data.clear();
        return (error.kind() != io::ErrorKind::WouldBlock).then_some(Err(error));
    }
    data.truncate(count as usize);
    Some(Ok(count as usize))
}

async fn receive_ancillary_worker(
    stream: Rc<compio::net::TcpStream>,
    buffers: SharedIoBuffers,
    events: flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
) {
    let owned_stream = Rc::clone(&stream);
    let mut stream = stream.as_ref();
    use compio::io::ancillary::{AsyncReadAncillary, ReturnFlags};

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
        if !buffers.borrow().ktls_active {
            match receive_before_ktls(&owned_stream, &buffers, room, &mut data).await {
                None => {
                    // Not readable after all, or handed off meanwhile: retry
                    // without waiting for another protocol visit.
                    buffers.borrow_mut().receive_armed = true;
                }
                Some(Ok(0)) => {
                    buffers.borrow_mut().eof = true;
                    let _ = notify_readable(&events, event).await;
                    return;
                }
                Some(Ok(count)) => {
                    {
                        let mut state = buffers.borrow_mut();
                        debug_assert!(count <= room);
                        state.received.extend(data[..count].iter().copied());
                        debug_assert!(state.received.len() <= TRANSPORT_BUFFER_CAPACITY);
                    }
                    data.clear();
                    if !notify_readable(&events, event).await {
                        return;
                    }
                }
                Some(Err(error)) => {
                    store_receive_error(&buffers, error);
                    let _ = notify_readable(&events, event).await;
                    return;
                }
            }
            continue;
        }
        if data.capacity() != room {
            data = Vec::with_capacity(room);
        }
        let compio::BufResult(result, (returned_data, returned_control)) =
            (&mut stream).read_with_ancillary(data, control).await;
        data = returned_data;
        control = returned_control;
        match result {
            Ok((0, _, _)) => {
                buffers.borrow_mut().eof = true;
                let _ = notify_readable(&events, event).await;
                return;
            }
            Ok((count, control_len, flags)) => {
                let ktls_active = buffers.borrow().ktls_active;
                let record_type = if ktls_active {
                    match super::super::rtmp_connection::rtmp_ktls::record_type_from_control(
                        &control[..control_len],
                        flags.contains(ReturnFlags::CTRUNC),
                    ) {
                        Ok(record_type) => Some(record_type),
                        Err(error) => {
                            store_receive_error(&buffers, error);
                            let _ = notify_readable(&events, event).await;
                            return;
                        }
                    }
                } else if control_len != 0 || flags.contains(ReturnFlags::CTRUNC) {
                    store_receive_error(
                        &buffers,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected RTMPS ancillary control data",
                        ),
                    );
                    let _ = notify_readable(&events, event).await;
                    return;
                } else {
                    None
                };
                {
                    let mut state = buffers.borrow_mut();
                    debug_assert!(count <= room);
                    state.received.extend(data[..count].iter().copied());
                    debug_assert!(state.received.len() <= TRANSPORT_BUFFER_CAPACITY);
                    if let Some(record_type) = record_type {
                        state.record_type = Some((count, record_type));
                    }
                }
                data.clear();
                control.clear();
                control.resize(24, 0);
                if !notify_readable(&events, event).await {
                    return;
                }
            }
            Err(error) => {
                store_receive_error(&buffers, error);
                let _ = notify_readable(&events, event).await;
                return;
            }
        }
    }
}

pub(super) async fn transmit_worker(
    stream: Rc<compio::net::TcpStream>,
    buffers: SharedIoBuffers,
    events: flume::Sender<TcpReadyLeaf>,
    event: TcpReadyLeaf,
) {
    let mut stream = stream.as_ref();
    use compio::io::AsyncWrite;
    let mut data = Vec::with_capacity(IO_CHUNK);
    loop {
        take_transmit(&buffers, &mut data).await;
        let mut offset = 0;
        while offset < data.len() {
            let compio::BufResult(result, returned) = stream.write(std::mem::take(&mut data)).await;
            data = returned;
            match result {
                Ok(0) => {
                    {
                        let mut state = buffers.borrow_mut();
                        state.error = Some((
                            io::ErrorKind::WriteZero,
                            "RTMP socket write returned zero".into(),
                        ));
                    }
                    let _ = events
                        .send_async(TcpReadyLeaf {
                            writable: true,
                            ..event
                        })
                        .await;
                    return;
                }
                Ok(count) => {
                    offset += count;
                    let partial = offset < data.len();
                    {
                        let mut state = buffers.borrow_mut();
                        state.pending_write_bytes = state.pending_write_bytes.saturating_sub(count);
                        if partial {
                            for byte in data[offset..].iter().rev() {
                                state.outgoing.push_front(*byte);
                            }
                        }
                    }
                    if partial {
                        data.clear();
                    }
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
                    if partial {
                        break;
                    }
                }
                Err(error) => {
                    {
                        let mut state = buffers.borrow_mut();
                        state.error = Some((error.kind(), error.to_string()));
                    }
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
