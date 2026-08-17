use std::collections::HashMap;
use std::os::fd::RawFd;
use std::time::Duration;

use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

use crate::media::egress::scheduler::LeafKey;
use crate::media::srt::{SrtEgressInterest, SrtEgressPollError, SrtLeafHandle, SrtReadyLeaf};

#[derive(Clone, Copy)]
struct Registration {
    key: LeafKey,
    generation: u64,
    fd: RawFd,
}

pub(crate) struct SrtRustFabricPoller {
    poll: Poll,
    events: Events,
    next_token: usize,
    by_handle: HashMap<SrtLeafHandle, Token>,
    by_token: HashMap<Token, Registration>,
}

impl SrtRustFabricPoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, SrtEgressPollError> {
        Poll::new()
            .map(|poll| Self {
                poll,
                events: Events::with_capacity(max_events.max(1)),
                next_token: 0,
                by_handle: HashMap::new(),
                by_token: HashMap::new(),
            })
            .map_err(|error| error_for("mio_poll_create", error))
    }

    pub(crate) fn register(
        &mut self,
        handle: SrtLeafHandle,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        let fd = rust_fd(handle)?;
        let token = self.by_handle.get(&handle).copied().unwrap_or_else(|| {
            let token = Token(self.next_token);
            self.next_token = self.next_token.wrapping_add(1);
            token
        });
        let mio_interest = mio_interest(interest);
        let mut source = SourceFd(&fd);
        let result = if self.by_handle.contains_key(&handle) {
            self.poll
                .registry()
                .reregister(&mut source, token, mio_interest)
        } else {
            self.poll
                .registry()
                .register(&mut source, token, mio_interest)
        };
        result.map_err(|error| error_for("mio_poll_register", error))?;
        self.by_handle.insert(handle, token);
        self.by_token.insert(
            token,
            Registration {
                key,
                generation,
                fd,
            },
        );
        Ok(())
    }

    pub(crate) fn remove(&mut self, handle: SrtLeafHandle) -> Result<(), SrtEgressPollError> {
        let Some(token) = self.by_handle.remove(&handle) else {
            return Ok(());
        };
        let Some(registration) = self.by_token.remove(&token) else {
            return Ok(());
        };
        let mut source = SourceFd(&registration.fd);
        self.poll
            .registry()
            .deregister(&mut source)
            .map_err(|error| error_for("mio_poll_remove", error))
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        let timeout = (timeout_ms >= 0).then(|| Duration::from_millis(timeout_ms as u64));
        self.poll
            .poll(&mut self.events, timeout)
            .map_err(|error| error_for("mio_poll_wait", error))?;
        for event in &self.events {
            let Some(registration) = self.by_token.get(&event.token()).copied() else {
                continue;
            };
            ready.push(SrtReadyLeaf {
                handle: SrtLeafHandle::Rust(registration.fd),
                key: registration.key,
                generation: registration.generation,
                readable: event.is_readable(),
                writable: event.is_writable(),
            });
        }
        Ok(ready.len())
    }
}

fn rust_fd(handle: SrtLeafHandle) -> Result<RawFd, SrtEgressPollError> {
    match handle {
        SrtLeafHandle::Rust(fd) => Ok(fd),
        SrtLeafHandle::Native(_) => Err(SrtEgressPollError {
            operation: "mio_poll_register",
            code: -1,
            message: "Rust SRT poller received a native handle".to_string(),
        }),
    }
}

fn mio_interest(interest: SrtEgressInterest) -> Interest {
    match (interest.readable, interest.writable) {
        (true, true) => Interest::READABLE | Interest::WRITABLE,
        (true, false) => Interest::READABLE,
        (false, true) => Interest::WRITABLE,
        (false, false) => Interest::READABLE,
    }
}

fn error_for(operation: &'static str, error: std::io::Error) -> SrtEgressPollError {
    SrtEgressPollError {
        operation,
        code: error.raw_os_error().unwrap_or(-1),
        message: error.to_string(),
    }
}
