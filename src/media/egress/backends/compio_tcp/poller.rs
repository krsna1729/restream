use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use compio::driver::{DriverType, ProactorBuilder};
use compio::runtime::fd::PollFd;
use compio::runtime::{Runtime, RuntimeBuilder};

use super::super::tcp::{TcpConnectAttempt, TcpEgressInterest, TcpEgressPollError, TcpReadyLeaf};
use super::CompioTcpStream;
#[cfg(test)]
use super::stream::TRANSPORT_BUFFER_CAPACITY;
use super::stream::{SharedIoBuffers, receive_worker, transmit_worker};
use crate::media::egress::scheduler::LeafKey;
use crate::media::egress::shard::EgressShardIdleWake;
impl Drop for CompioTcpPoller {
    fn drop(&mut self) {
        for tasks in self.io_tasks.values() {
            tasks.receive.abort.abort();
            tasks.transmit.abort.abort();
        }
        let tasks = std::mem::take(&mut self.io_tasks);
        self.runtime.block_on(async move {
            for (_, tasks) in tasks {
                let _ = tasks.receive.task.await;
                let _ = tasks.transmit.task.await;
            }
        });
    }
}
struct IoTask {
    abort: futures_util::future::AbortHandle,
    task: compio::runtime::JoinHandle<Result<(), futures_util::future::Aborted>>,
}
struct IoTasks {
    key: LeafKey,
    generation: u64,
    receive: IoTask,
    transmit: IoTask,
}
struct Registration {
    stream: PollFd<compio::net::TcpStream>,
    fd: RawFd,
    key: LeafKey,
    generation: u64,
    interest: TcpEgressInterest,
}
type CompioReadyEvent = (RawFd, LeafKey, u64, bool, bool);
/// One Compio owner for one RTMP fabric shard. Connecting sockets alone use
/// `PollFd`; established sockets are driven only by bounded completions.
///
/// The ring keeps compio's default size, as the pre-completion poller did.
/// SQ size bounds submissions per `io_uring_enter`, not in-flight operations:
/// compio submits and reaps when the SQ is full. Sizing it per leaf slot
/// (8192 SQ / 16384 CQ) made ring setup fail with ENOMEM once a host ran many
/// RTMP feeds, each with its own shards.
pub(crate) struct CompioTcpPoller {
    runtime: Runtime,
    registrations: HashMap<RawFd, Registration>,
    registration_order: Vec<RawFd>,
    next_registration: usize,
    ready_queue: VecDeque<CompioReadyEvent>,
    io_tasks: HashMap<RawFd, IoTasks>,
    event_tx: flume::Sender<TcpReadyLeaf>,
    event_rx: flume::Receiver<TcpReadyLeaf>,
    ready_capacity: usize,
    /// I/O completion events consumed, and those dropped as stale (an older
    /// generation or a removed connection), for shard metrics.
    completions: u64,
    stale_completions: u64,
}

impl CompioTcpPoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, TcpEgressPollError> {
        let ready_capacity = max_events.max(1);
        let mut proactor = ProactorBuilder::new();
        proactor.driver_type(DriverType::IoUring);
        let mut runtime_builder = RuntimeBuilder::new();
        runtime_builder.with_proactor(proactor);
        let runtime = runtime_builder
            .build()
            .map_err(|error| Self::error("compio_tcp_io_uring_runtime", error))?;
        if !runtime.driver_type().is_iouring() {
            return Err(TcpEgressPollError::new(
                "compio_tcp_runtime_driver",
                libc::ENOTSUP,
                format!(
                    "RTMP egress requires io_uring, got {:?}",
                    runtime.driver_type()
                ),
            ));
        }
        let (event_tx, event_rx) = flume::bounded(ready_capacity);
        Ok(Self {
            runtime,
            registrations: HashMap::with_capacity(ready_capacity),
            registration_order: Vec::with_capacity(ready_capacity),
            next_registration: 0,
            ready_queue: VecDeque::with_capacity(ready_capacity),
            io_tasks: HashMap::with_capacity(ready_capacity),
            event_tx,
            event_rx,
            ready_capacity,
            completions: 0,
            stale_completions: 0,
        })
    }

    pub(crate) fn ready_capacity(&self) -> usize {
        self.ready_capacity
    }

    /// `(consumed, stale)` I/O completion events since creation.
    pub(crate) fn completion_counts(&self) -> (u64, u64) {
        (self.completions, self.stale_completions)
    }
    fn completion_current(&self, event: &TcpReadyLeaf) -> bool {
        self.io_tasks
            .get(&event.fd)
            .is_some_and(|tasks| tasks.key == event.key && tasks.generation == event.generation)
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
    fn start_io(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        stream: Rc<compio::net::TcpStream>,
        buffers: SharedIoBuffers,
    ) -> Result<(), TcpEgressPollError> {
        if self.io_tasks.contains_key(&fd) {
            return Ok(());
        }
        let event = TcpReadyLeaf {
            fd,
            key,
            generation,
            readable: false,
            writable: false,
        };
        let (receive_abort, receive_registration) = futures_util::future::AbortHandle::new_pair();
        let receive_events = self.event_tx.clone();
        let receive_buffers = buffers.clone();
        let receive_task = self.runtime.enter(|| {
            compio::runtime::spawn(futures_util::future::Abortable::new(
                receive_worker(stream.clone(), receive_buffers, receive_events, event),
                receive_registration,
            ))
        });
        let (transmit_abort, transmit_registration) = futures_util::future::AbortHandle::new_pair();
        let transmit_events = self.event_tx.clone();
        let transmit_task = self.runtime.enter(|| {
            compio::runtime::spawn(futures_util::future::Abortable::new(
                transmit_worker(stream, buffers, transmit_events, event),
                transmit_registration,
            ))
        });
        self.io_tasks.insert(
            fd,
            IoTasks {
                key,
                generation,
                receive: IoTask {
                    abort: receive_abort,
                    task: receive_task,
                },
                transmit: IoTask {
                    abort: transmit_abort,
                    task: transmit_task,
                },
            },
        );
        self.runtime.poll_with(Some(Duration::ZERO));
        Ok(())
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
            .map_err(|error| Self::error("compio_tcp_connect_register", error))?;
        let stream = self
            .runtime
            .enter(|| compio::net::TcpStream::from_std(stream))
            .map_err(|error| Self::error("compio_tcp_connect_register", error))?;
        let poll = PollFd::new(stream)
            .map_err(|error| Self::error("compio_tcp_connect_register", error))?;
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

    pub(crate) fn register_connection(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        transport: &CompioTcpStream,
    ) -> Result<(), TcpEgressPollError> {
        if self
            .io_tasks
            .get(&fd)
            .is_some_and(|tasks| tasks.key == key && tasks.generation == generation)
        {
            return Ok(());
        }
        let Some(stream) = transport.io_stream() else {
            return Err(TcpEgressPollError::new(
                "compio_tcp_connection_stream",
                libc::EINVAL,
                "RTMP completion transport has no Compio stream".to_owned(),
            ));
        };
        let Some(buffers) = transport.io_buffers() else {
            return Err(TcpEgressPollError::new(
                "compio_tcp_connection_buffers",
                libc::EINVAL,
                "RTMP completion transport has no I/O buffers".to_owned(),
            ));
        };
        self.remove(fd)?;
        self.start_io(fd, key, generation, stream, buffers)
    }

    pub(crate) fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        if let Some(tasks) = self.io_tasks.remove(&fd) {
            tasks.receive.abort.abort();
            tasks.transmit.abort.abort();
            self.runtime.block_on(async {
                let _ = tasks.receive.task.await;
                let _ = tasks.transmit.task.await;
            });
        }
        self.registrations.remove(&fd);
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
        let connect_budget = (self.ready_capacity / 2).max(1);
        let scan_capacity = self
            .ready_capacity
            .saturating_sub(self.ready_queue.len())
            .min(connect_budget);
        if scan_capacity > 0 && !self.registration_order.is_empty() {
            let runtime = &self.runtime;
            let registrations = &self.registrations;
            let registration_order = &self.registration_order;
            let next_registration = &mut self.next_registration;
            let ready_queue = &mut self.ready_queue;
            let scan_result = runtime.block_on(std::future::poll_fn(|cx| {
                Poll::Ready(
                    match poll_ready_batch(
                        registrations,
                        registration_order,
                        next_registration,
                        ready_queue,
                        scan_capacity,
                        scan_capacity,
                        cx,
                    ) {
                        Poll::Ready(result) => Some(result),
                        Poll::Pending => None,
                    },
                )
            }));
            if let Some(Err(error)) = scan_result {
                return Err(Self::error("compio_tcp_poll", error));
            }
        }

        while ready.len() < connect_budget {
            let Some((fd, key, generation, readable, writable)) = self.ready_queue.pop_front()
            else {
                break;
            };
            ready.push(TcpReadyLeaf {
                fd,
                key,
                generation,
                readable,
                writable,
            });
        }
        while ready.len() < self.ready_capacity {
            match self.event_rx.try_recv() {
                Ok(event) if self.completion_current(&event) => {
                    self.completions += 1;
                    ready.push(event);
                }
                Ok(_) => self.stale_completions += 1,
                Err(_) => break,
            }
        }
        if ready.is_empty() {
            if timeout_ms <= 0 {
                self.drive_nonblocking();
            } else {
                self.wait_one(Duration::from_millis(timeout_ms as u64))?;
            }
            while ready.len() < self.ready_capacity {
                let Some((fd, key, generation, readable, writable)) = self.ready_queue.pop_front()
                else {
                    break;
                };
                ready.push(TcpReadyLeaf {
                    fd,
                    key,
                    generation,
                    readable,
                    writable,
                });
            }
            while ready.len() < self.ready_capacity {
                match self.event_rx.try_recv() {
                    Ok(event) if self.completion_current(&event) => {
                        self.completions += 1;
                        ready.push(event);
                    }
                    Ok(_) => self.stale_completions += 1,
                    Err(_) => break,
                }
            }
        }
        Ok(ready.len())
    }

    pub(crate) fn wait_idle(
        &mut self,
        commands: &flume::Receiver<crate::media::egress::command::EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        enum Wake {
            Command(Box<crate::media::egress::command::EgressCommand>),
            Completion(TcpReadyLeaf),
            Activity,
            Timeout,
            Disconnected,
        }
        if !self.ready_queue.is_empty() || !self.event_rx.is_empty() {
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
                    Ok(command) => Wake::Command(Box::new(command)),
                    Err(_) => Wake::Disconnected,
                }
            };
            let activity = async {
                let result = std::future::poll_fn(|cx| {
                    poll_ready_batch(
                        registrations,
                        registration_order,
                        next_registration,
                        ready_queue,
                        ready_capacity,
                        ready_capacity,
                        cx,
                    )
                })
                .await;
                match result {
                    Ok(()) => Wake::Activity,
                    Err(error) => {
                        tracing::warn!(%error, "Compio RTMP connect completion wait failed");
                        Wake::Activity
                    }
                }
            };
            let completion = async {
                match self.event_rx.recv_async().await {
                    Ok(event) => Wake::Completion(event),
                    Err(_) => Wake::Disconnected,
                }
            };
            let timeout = async {
                compio::time::sleep(max_wait).await;
                Wake::Timeout
            };
            runtime.block_on(async {
                tokio::select! {
                    biased;
                    wake = activity => wake,
                    wake = completion => wake,
                    wake = command => wake,
                    wake = timeout => wake,
                }
            })
        };
        match wake {
            Wake::Command(command) => {
                self.runtime.poll_with(Some(Duration::ZERO));
                EgressShardIdleWake::Command(*command)
            }
            Wake::Completion(event) => {
                if self.completion_current(&event) {
                    self.completions += 1;
                    self.ready_queue.push_back((
                        event.fd,
                        event.key,
                        event.generation,
                        event.readable,
                        event.writable,
                    ));
                    EgressShardIdleWake::BackendActivity
                } else {
                    self.stale_completions += 1;
                    EgressShardIdleWake::Timeout
                }
            }
            Wake::Activity => EgressShardIdleWake::BackendActivity,
            Wake::Timeout => EgressShardIdleWake::Timeout,
            Wake::Disconnected => EgressShardIdleWake::Disconnected,
        }
    }

    /// Advance socket I/O without blocking: run woken workers so their
    /// operations reach the submission queue, submit and reap completions
    /// with a zero-timeout driver poll, then run the workers those
    /// completions woke. Each executor tick is bounded by compio's
    /// `max_interval`. `block_on(timeout(ZERO, ..))` is not equivalent: its
    /// zero sleep resolves on the first poll, so it returns after one tick
    /// without submitting or reaping any I/O.
    fn drive_nonblocking(&mut self) {
        let runtime = &self.runtime;
        runtime.enter(|| {
            runtime.run();
            runtime.poll_with(Some(Duration::ZERO));
            runtime.run();
        });
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
    scan_budget: usize,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    let count = registration_order.len();
    if count == 0 || scan_budget == 0 {
        return Poll::Pending;
    }

    let mut index = *next_registration % count;
    let mut scanned = 0;
    while scanned < count && scanned < scan_budget && ready.len() < capacity {
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
#[path = "poller_tests.rs"]
mod tests;
