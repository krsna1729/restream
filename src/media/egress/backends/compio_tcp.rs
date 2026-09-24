use std::collections::{HashMap, VecDeque};
use std::io::{self, IoSlice, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::task::{Context, Poll};
use std::time::Duration;

use compio::runtime::fd::PollFd;
use futures_util::future::{Either, FutureExt, select};
use futures_util::pin_mut;

use crate::media::egress::scheduler::LeafKey;
use crate::media::egress::shard::EgressShardIdleWake;

use super::tcp::{TcpConnectAttempt, TcpEgressInterest, TcpEgressPollError, TcpReadyLeaf};

/// A TCP stream whose descriptor is owned by Compio while the protocol engine
/// drives nonblocking reads and writes through the standard I/O traits.
///
/// RTMP's state machine is deliberately synchronous: one shard visit advances
/// the protocol until the descriptor would block. Production sockets remain
/// Compio `TcpStream`s; the std-I/O adapter only performs the already-ready
/// syscall against that owned descriptor. The `Std` variant is test-only
/// compatibility for protocol unit tests that do not run a Compio runtime.
#[derive(Debug)]
pub(crate) enum CompioTcpStream {
    Compio(compio::net::TcpStream),
    #[cfg(test)]
    Std(TcpStream),
}

impl CompioTcpStream {
    #[cfg(test)]
    pub(crate) fn from_std(stream: TcpStream) -> Self {
        Self::Std(stream)
    }

    pub(crate) fn from_compio(stream: compio::net::TcpStream) -> Self {
        Self::Compio(stream)
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

    pub(crate) fn raw_fd(&self) -> RawFd {
        match self {
            Self::Compio(stream) => stream.as_raw_fd(),
            #[cfg(test)]
            Self::Std(stream) => stream.as_raw_fd(),
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
        let result = unsafe { libc::recv(self.raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }
}

impl Write for CompioTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let result =
            unsafe { libc::send(self.raw_fd(), buf.as_ptr().cast(), buf.len(), send_flags()) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let mut iovecs = [libc::iovec {
            iov_base: std::ptr::null_mut(),
            iov_len: 0,
        }; 16];
        let mut count = 0;
        for buffer in bufs {
            if buffer.is_empty() {
                continue;
            }
            if count == iovecs.len() {
                break;
            }
            iovecs[count] = libc::iovec {
                iov_base: buffer.as_ptr().cast_mut().cast(),
                iov_len: buffer.len(),
            };
            count += 1;
        }
        if count == 0 {
            return Ok(0);
        }
        let result = unsafe {
            libc::sendmsg(
                self.raw_fd(),
                &libc::msghdr {
                    msg_name: std::ptr::null_mut(),
                    msg_namelen: 0,
                    msg_iov: iovecs.as_mut_ptr(),
                    msg_iovlen: count,
                    msg_control: std::ptr::null_mut(),
                    msg_controllen: 0,
                    msg_flags: 0,
                },
                send_flags(),
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const fn send_flags() -> libc::c_int {
    libc::MSG_NOSIGNAL
}

#[cfg(not(target_os = "linux"))]
const fn send_flags() -> libc::c_int {
    0
}

struct Registration {
    stream: PollFd<compio::net::TcpStream>,
    fd: RawFd,
    key: LeafKey,
    generation: u64,
    interest: TcpEgressInterest,
}
type CompioReadyEvent = (RawFd, LeafKey, u64, bool, bool);
/// One Compio readiness owner for one RTMP fabric shard.
///
/// Each `PollFd` retains its own readiness submissions. A rotating scan fills
/// a bounded ready queue without rebuilding one future per registered socket.
/// The protocol engine still receives generation-tagged events, so scheduler,
/// retries, drain deadlines, and slow-peer isolation stay unchanged.
pub(crate) struct CompioTcpPoller {
    runtime: compio::runtime::Runtime,
    registrations: HashMap<RawFd, Registration>,
    registration_order: Vec<RawFd>,
    next_registration: usize,
    ready_queue: VecDeque<CompioReadyEvent>,
    ready_capacity: usize,
}

impl CompioTcpPoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, TcpEgressPollError> {
        let runtime = compio::runtime::Runtime::new()
            .map_err(|error| Self::error("compio_tcp_runtime", error))?;
        let ready_capacity = max_events.max(1);
        Ok(Self {
            runtime,
            registrations: HashMap::with_capacity(ready_capacity),
            registration_order: Vec::with_capacity(ready_capacity),
            next_registration: 0,
            ready_queue: VecDeque::with_capacity(ready_capacity),
            ready_capacity,
        })
    }

    pub(crate) fn ready_capacity(&self) -> usize {
        self.ready_capacity
    }

    pub(crate) fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        _timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError> {
        let stream = open_nonblocking_tcp(peer_addr)?;
        let fd = stream.as_raw_fd();
        let (address, address_len) = socket_address(peer_addr);
        let result = unsafe {
            libc::connect(
                fd,
                (&address as *const libc::sockaddr_storage).cast(),
                address_len,
            )
        };
        if result == 0 {
            return Ok(TcpConnectAttempt::Connected(self.adopt(stream)?));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(Self::error("compio_tcp_connect", error));
        }
        let stream = self.adopt(stream)?;
        self.register_leaf(fd, key, generation, TcpEgressInterest::WRITE)?;
        Ok(TcpConnectAttempt::InProgress(stream))
    }

    fn adopt(&self, stream: TcpStream) -> Result<CompioTcpStream, TcpEgressPollError> {
        self.runtime
            .enter(|| compio::net::TcpStream::from_std(stream))
            .map(CompioTcpStream::from_compio)
            .map_err(|error| Self::error("compio_tcp_adopt", error))
    }

    pub(crate) fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        self.remove(fd)?;
        let stream = CompioTcpStream::duplicate_std_fd(fd)
            .map_err(|error| Self::error("compio_tcp_register", error))?;
        let stream = self
            .runtime
            .enter(|| compio::net::TcpStream::from_std(stream))
            .map_err(|error| Self::error("compio_tcp_register", error))?;
        let poll =
            PollFd::new(stream).map_err(|error| Self::error("compio_tcp_register", error))?;
        self.registrations.insert(
            fd,
            Registration {
                stream: poll,
                fd,
                key,
                generation,
                interest,
            },
        );
        self.registration_order.push(fd);
        Ok(())
    }

    pub(crate) fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        if self.registrations.remove(&fd).is_none() {
            return Ok(());
        }
        self.ready_queue.retain(|event| event.0 != fd);
        if let Some(index) = self
            .registration_order
            .iter()
            .position(|registered| *registered == fd)
        {
            let last = self.registration_order.len() - 1;
            self.registration_order.swap_remove(index);
            if self.registration_order.is_empty() {
                self.next_registration = 0;
            } else if self.next_registration == index || self.next_registration == last {
                self.next_registration = index % self.registration_order.len();
            } else {
                self.next_registration %= self.registration_order.len();
            }
        }
        Ok(())
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        ready.clear();
        if self.ready_queue.is_empty() {
            self.wait_one(Duration::from_millis(timeout_ms.max(0) as u64))?;
        }
        let count = self.ready_queue.len().min(self.ready_capacity);
        for _ in 0..count {
            if let Some((fd, key, generation, readable, writable)) = self.ready_queue.pop_front() {
                ready.push(TcpReadyLeaf {
                    fd,
                    key,
                    generation,
                    readable,
                    writable,
                });
            }
        }
        Ok(ready.len())
    }

    pub(crate) fn wait_idle(
        &mut self,
        commands: &flume::Receiver<crate::media::egress::command::EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        if !self.ready_queue.is_empty() {
            return EgressShardIdleWake::BackendActivity;
        }
        let wake = {
            let runtime = &self.runtime;
            let registrations = &self.registrations;
            let registration_order = &self.registration_order;
            let next_registration = &mut self.next_registration;
            let ready_queue = &mut self.ready_queue;
            let ready_capacity = self.ready_capacity;
            let command = async {
                match commands.recv_async().await {
                    Ok(command) => EgressShardIdleWake::Command(command),
                    Err(_) => EgressShardIdleWake::Disconnected,
                }
            };
            let activity = async {
                match std::future::poll_fn(|cx| {
                    poll_ready_batch(
                        registrations,
                        registration_order,
                        next_registration,
                        ready_queue,
                        ready_capacity,
                        cx,
                    )
                })
                .await
                {
                    Ok(()) => EgressShardIdleWake::BackendActivity,
                    Err(error) => {
                        tracing::warn!(%error, "Compio RTMP readiness wait failed");
                        EgressShardIdleWake::BackendActivity
                    }
                }
            };
            let timeout = async {
                compio::time::sleep(max_wait).await;
                EgressShardIdleWake::Timeout
            };
            let command = command.fuse();
            let activity = activity.fuse();
            let timeout = timeout.fuse();
            pin_mut!(command, activity, timeout);
            runtime.block_on(async {
                match select(command, select(activity, timeout)).await {
                    Either::Left((wake, _)) => wake,
                    Either::Right((Either::Left((wake, _)), _)) => wake,
                    Either::Right((Either::Right((wake, _)), _)) => wake,
                }
            })
        };
        if matches!(&wake, EgressShardIdleWake::Command(_)) && !self.registrations.is_empty() {
            // Commands must not keep pending PollFd submissions from progressing.
            let readiness = {
                let runtime = &self.runtime;
                let registrations = &self.registrations;
                let registration_order = &self.registration_order;
                let next_registration = &mut self.next_registration;
                let ready_queue = &mut self.ready_queue;
                let ready_capacity = self.ready_capacity;
                runtime.block_on(async {
                    std::future::poll_fn(|cx| {
                        poll_ready_batch(
                            registrations,
                            registration_order,
                            next_registration,
                            ready_queue,
                            ready_capacity,
                            cx,
                        )
                    })
                    .now_or_never()
                })
            };
            if let Some(Err(error)) = readiness {
                tracing::warn!(%error, "Compio RTMP readiness wait failed");
            }
            self.runtime.poll_with(Some(Duration::ZERO));
        }
        wake
    }

    fn wait_one(&mut self, timeout: Duration) -> Result<(), TcpEgressPollError> {
        let runtime = &self.runtime;
        let registrations = &self.registrations;
        let registration_order = &self.registration_order;
        let next_registration = &mut self.next_registration;
        let ready_queue = &mut self.ready_queue;
        let ready_capacity = self.ready_capacity;
        match runtime.block_on(compio::time::timeout(
            timeout,
            std::future::poll_fn(|cx| {
                poll_ready_batch(
                    registrations,
                    registration_order,
                    next_registration,
                    ready_queue,
                    ready_capacity,
                    cx,
                )
            }),
        )) {
            Err(_) => Ok(()),
            Ok(Err(error)) => Err(Self::error("compio_tcp_poll", error)),
            Ok(Ok(())) => Ok(()),
        }
    }

    fn error(operation: &'static str, error: io::Error) -> TcpEgressPollError {
        TcpEgressPollError::new(
            operation,
            error.raw_os_error().unwrap_or(libc::EIO),
            error.to_string(),
        )
    }
}

fn poll_ready_batch(
    registrations: &HashMap<RawFd, Registration>,
    registration_order: &[RawFd],
    next_registration: &mut usize,
    ready: &mut VecDeque<CompioReadyEvent>,
    capacity: usize,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    let count = registration_order.len();
    if count == 0 {
        return Poll::Pending;
    }

    let mut index = *next_registration % count;
    let mut scanned = 0;
    while scanned < count && ready.len() < capacity {
        if let Some(registration) = registrations.get(&registration_order[index]) {
            let mut readable = false;
            let mut writable = false;
            if registration.interest.readable {
                match registration.stream.poll_read_ready(cx) {
                    Poll::Ready(Ok(())) => readable = true,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => {}
                }
            }
            if registration.interest.writable {
                match registration.stream.poll_write_ready(cx) {
                    Poll::Ready(Ok(())) => writable = true,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => {}
                }
            }
            if readable || writable {
                ready.push_back((
                    registration.fd,
                    registration.key,
                    registration.generation,
                    readable,
                    writable,
                ));
            }
        }
        scanned += 1;
        index += 1;
        if index == count {
            index = 0;
        }
    }
    *next_registration = index;
    if ready.is_empty() {
        Poll::Pending
    } else {
        Poll::Ready(Ok(()))
    }
}

fn open_nonblocking_tcp(peer_addr: SocketAddr) -> Result<TcpStream, TcpEgressPollError> {
    let domain = match peer_addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    let fd = unsafe {
        libc::socket(
            domain,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::IPPROTO_TCP,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return Err(CompioTcpPoller::error("compio_tcp_socket", error));
    }
    let stream = unsafe { TcpStream::from_raw_fd(fd) };
    stream
        .set_nodelay(true)
        .map_err(|error| CompioTcpPoller::error("compio_tcp_nodelay", error))?;
    Ok(stream)
}

fn socket_address(peer_addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match peer_addr {
        SocketAddr::V4(address) => {
            let value = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: address.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&value as *const libc::sockaddr_in).cast::<u8>(),
                    (&mut storage as *mut libc::sockaddr_storage).cast::<u8>(),
                    std::mem::size_of::<libc::sockaddr_in>(),
                );
            }
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(address) => {
            let value = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: address.port().to_be(),
                sin6_flowinfo: address.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: address.ip().octets(),
                },
                sin6_scope_id: address.scope_id(),
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (&value as *const libc::sockaddr_in6).cast::<u8>(),
                    (&mut storage as *mut libc::sockaddr_storage).cast::<u8>(),
                    std::mem::size_of::<libc::sockaddr_in6>(),
                );
            }
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompioTcpPoller, CompioTcpStream, TcpConnectAttempt};
    use crate::media::egress::scheduler::LeafKey;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn compio_poller_reports_generation_tagged_connect_readiness() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || listener.accept().unwrap());
        let key = LeafKey(3);
        let mut poller = CompioTcpPoller::new(4).unwrap();
        let attempt = poller
            .start_connect(address, key, 7, Duration::from_secs(2))
            .unwrap();

        let (stream, ready) = match attempt {
            TcpConnectAttempt::Connected(stream) => (stream, None),
            TcpConnectAttempt::InProgress(stream) => {
                let mut events = Vec::new();
                assert_eq!(poller.poll_leaves(2_000, &mut events).unwrap(), 1);
                (stream, Some(events[0]))
            }
        };
        if let Some(event) = ready {
            assert_eq!(event.fd, stream.raw_fd());
            assert_eq!((event.key, event.generation), (key, 7));
            crate::media::egress::backends::tcp::connect_error(event.fd).unwrap();
            poller.remove(event.fd).unwrap();
        }
        poller
            .register_leaf(stream.raw_fd(), key, 8, super::TcpEgressInterest::WRITE)
            .unwrap();
        let (_command_tx, commands) = flume::unbounded();
        assert!(matches!(
            poller.wait_idle(&commands, Duration::from_secs(2)),
            crate::media::egress::shard::EgressShardIdleWake::BackendActivity
        ));
        let mut events = Vec::new();
        assert_eq!(
            poller.poll_leaves(0, &mut events).unwrap(),
            1,
            "idle-wait readiness must be preserved for the scheduled shard visit"
        );
        assert_eq!(events[0].generation, 8);
        assert_eq!(events[0].key, key);
        assert!(events[0].writable);
        poller.remove(stream.raw_fd()).unwrap();
        drop(stream);
        let (accepted, _) = server.join().unwrap();
        drop(accepted);
    }
    #[test]
    fn command_and_socket_readiness_are_serviced_fairly() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || listener.accept().unwrap());
        let mut poller = CompioTcpPoller::new(4).unwrap();
        let client = TcpStream::connect(address).unwrap();
        client.set_nonblocking(true).unwrap();
        let stream = poller.adopt(client).unwrap();
        let fd = stream.raw_fd();
        let key = LeafKey(9);
        poller
            .register_leaf(fd, key, 11, super::TcpEgressInterest::WRITE)
            .unwrap();
        let (command_tx, commands) = flume::unbounded();
        const WAKE_COUNT: usize = 64;
        for _ in 0..WAKE_COUNT {
            command_tx
                .send(crate::media::egress::command::EgressCommand::FeedWake)
                .unwrap();
        }

        let mut commands_seen = 0;
        let mut readiness_seen = false;
        for _ in 0..4 {
            match poller.wait_idle(&commands, Duration::from_secs(2)) {
                crate::media::egress::shard::EgressShardIdleWake::BackendActivity => {
                    let mut events = Vec::new();
                    assert_eq!(poller.poll_leaves(0, &mut events).unwrap(), 1);
                    assert_eq!((events[0].key, events[0].generation), (key, 11));
                    assert!(events[0].writable);
                    readiness_seen = true;
                }
                crate::media::egress::shard::EgressShardIdleWake::Command(
                    crate::media::egress::command::EgressCommand::FeedWake,
                ) => commands_seen += 1,
                wake => panic!("unexpected idle wake: {wake:?}"),
            }
            if readiness_seen && commands_seen > 0 {
                break;
            }
        }
        assert!(readiness_seen);
        assert!(commands_seen > 0 && commands_seen < WAKE_COUNT);

        poller.remove(fd).unwrap();
        drop(stream);
        drop(command_tx);
        let (accepted, _) = server.join().unwrap();
        drop(accepted);
    }

    #[test]
    fn compio_poller_reports_simultaneous_read_and_write_readiness() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(b"x").unwrap();
        });
        let client = TcpStream::connect(address).unwrap();
        client.set_nonblocking(true).unwrap();
        let mut poller = CompioTcpPoller::new(4).unwrap();
        let stream = poller.adopt(client).unwrap();
        let fd = stream.raw_fd();
        let key = LeafKey(4);
        poller
            .register_leaf(fd, key, 13, super::TcpEgressInterest::READ_WRITE)
            .unwrap();
        server.join().unwrap();

        let mut events = Vec::new();
        assert_eq!(poller.poll_leaves(2_000, &mut events).unwrap(), 1);
        assert_eq!((events[0].key, events[0].generation), (key, 13));
        assert!(events[0].readable);
        assert!(events[0].writable);

        poller.remove(fd).unwrap();
        drop(stream);
    }

    #[test]
    fn nonblocking_compio_socket_supports_synchronous_protocol_io() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 5];
            stream.read_exact(&mut buffer).unwrap();
            stream.write_all(b"world").unwrap();
        });
        let client = TcpStream::connect(address).unwrap();
        client.set_nonblocking(true).unwrap();
        let runtime = compio::runtime::Runtime::new().unwrap();
        let client = runtime
            .enter(|| compio::net::TcpStream::from_std(client))
            .unwrap();
        let mut stream = CompioTcpStream::from_compio(client);
        stream.write_all(b"hello").unwrap();
        let mut response = [0; 5];
        loop {
            match stream.read(&mut response) {
                Ok(5) => break,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::yield_now(),
                Err(error) => panic!("read failed: {error}"),
            }
        }
        assert_eq!(&response, b"world");
        server.join().unwrap();
    }
    fn run_compio_poller_fanout(report_metrics: bool) {
        const REGISTRATIONS: usize = 96;
        const ACTIVE: usize = 24;
        const BATCH: usize = 8;
        const SAMPLES: usize = 20;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut poller = CompioTcpPoller::new(BATCH).unwrap();
        let mut clients = Vec::with_capacity(REGISTRATIONS);
        let mut peers = Vec::with_capacity(REGISTRATIONS);
        for index in 0..REGISTRATIONS {
            let client = TcpStream::connect(address).unwrap();
            let (peer, _) = listener.accept().unwrap();
            client.set_nonblocking(true).unwrap();
            let stream = poller.adopt(client).unwrap();
            poller
                .register_leaf(
                    stream.raw_fd(),
                    LeafKey(index),
                    7,
                    super::TcpEgressInterest::READ,
                )
                .unwrap();
            clients.push(stream);
            peers.push(peer);
        }

        let active = (0..REGISTRATIONS)
            .filter(|index| index % 4 == 0)
            .collect::<Vec<_>>();
        assert_eq!(active.len(), ACTIVE);
        let mut is_active = [false; REGISTRATIONS];
        for index in &active {
            is_active[*index] = true;
        }

        let mut elapsed_ns = Vec::with_capacity(SAMPLES);
        let mut poll_calls = Vec::with_capacity(SAMPLES);
        let mut allocation_counts = Vec::with_capacity(SAMPLES * ACTIVE);
        let mut ready = Vec::with_capacity(BATCH);
        assert_eq!(
            poller.poll_leaves(1, &mut ready).unwrap(),
            0,
            "warm persistent PollFd readiness registrations before measurement"
        );
        crate::test_alloc::begin();
        crate::test_alloc::end();
        for sample in 0..SAMPLES {
            for index in &active {
                peers[*index].write_all(&[sample as u8]).unwrap();
            }

            let started = std::time::Instant::now();
            let mut seen = [false; REGISTRATIONS];
            let mut seen_count = 0;
            let mut calls = 0;
            while seen_count < ACTIVE {
                calls += 1;
                assert!(
                    calls <= ACTIVE,
                    "active RTMP registrations were not serviced fairly"
                );
                crate::test_alloc::begin();
                let poll_result = poller.poll_leaves(1_000, &mut ready);
                let allocations = crate::test_alloc::end();
                allocation_counts.push(allocations);
                assert!(
                    allocations < REGISTRATIONS,
                    "{allocations} allocations for {REGISTRATIONS} registrations; per-registration readiness allocations regressed"
                );
                assert!(poll_result.unwrap() > 0);
                for event in ready.drain(..) {
                    let index = event.key.0;
                    assert!(is_active[index], "idle registration reported ready");
                    assert_eq!(event.generation, 7);
                    assert!(!seen[index], "ready registration was reported twice");
                    seen[index] = true;
                    seen_count += 1;
                    let mut byte = [0];
                    clients[index].read_exact(&mut byte).unwrap();
                    assert_eq!(byte[0], sample as u8);
                }
            }
            elapsed_ns.push(started.elapsed().as_nanos());
            poll_calls.push(calls);
        }

        if report_metrics {
            elapsed_ns.sort_unstable();
            allocation_counts.sort_unstable();
            poll_calls.sort_unstable();
            eprintln!(
                "[rtmp-poller-bench] registrations={REGISTRATIONS} active={ACTIVE} batch={BATCH} samples={SAMPLES} median_allocations_per_wait={} max_allocations_per_wait={} allocating_waits={} total_allocations={} median_ns={} p95_ns={} median_poll_calls={}",
                allocation_counts[allocation_counts.len() / 2],
                allocation_counts.last().copied().unwrap_or_default(),
                allocation_counts.iter().filter(|count| **count > 0).count(),
                allocation_counts.iter().sum::<usize>(),
                elapsed_ns[SAMPLES / 2],
                elapsed_ns[SAMPLES * 95 / 100],
                poll_calls[SAMPLES / 2],
            );
        }
    }

    #[test]
    fn compio_poller_fans_out_active_sockets_among_idle_registrations() {
        run_compio_poller_fanout(false);
    }

    #[test]
    #[ignore = "manual readiness latency and allocation benchmark"]
    fn compio_poller_fanout_benchmark() {
        run_compio_poller_fanout(true);
    }
}
