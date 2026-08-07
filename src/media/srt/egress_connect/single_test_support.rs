use super::*;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Event {
    Create,
    Timeout(SRTSOCKET, u64),
    EgressOpts(SRTSOCKET, EgressBufferOpts),
    ReuseAddr(SRTSOCKET),
    Crypto(SRTSOCKET),
    StreamId(SRTSOCKET, String),
    Bind(SRTSOCKET, u16),
    Connect(SRTSOCKET, SocketAddr),
    Configure(SRTSOCKET, SrtEgressSendMode),
    LocalPort(SRTSOCKET),
    Log(SRTSOCKET),
    Close(SRTSOCKET),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailStep {
    ReuseAddr,
    Crypto,
    StreamId,
    Bind,
    Connect,
    Configure,
}

pub(super) struct FakeSingleConnectOps {
    pub(super) events: RefCell<Vec<Event>>,
    fail_step: Option<FailStep>,
    socket: SRTSOCKET,
    local_port: u16,
}

impl FakeSingleConnectOps {
    pub(super) fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            fail_step: None,
            socket: 42,
            local_port: 41000,
        }
    }

    pub(super) fn failing(fail_step: FailStep) -> Self {
        Self {
            fail_step: Some(fail_step),
            ..Self::new()
        }
    }
}

impl SrtSingleConnectOps for &FakeSingleConnectOps {
    fn create_socket(&mut self) -> Result<SRTSOCKET, String> {
        self.events.borrow_mut().push(Event::Create);
        Ok(self.socket)
    }

    fn close(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Close(socket));
    }

    fn set_connect_timeout(&mut self, socket: SRTSOCKET, timeout_ms: u64) {
        self.events
            .borrow_mut()
            .push(Event::Timeout(socket, timeout_ms));
    }

    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts) {
        self.events
            .borrow_mut()
            .push(Event::EgressOpts(socket, *opts));
    }

    fn set_reuseaddr(&mut self, socket: SRTSOCKET) -> Result<(), String> {
        self.events.borrow_mut().push(Event::ReuseAddr(socket));
        if self.fail_step == Some(FailStep::ReuseAddr) {
            return Err("reuseaddr failed".to_string());
        }
        Ok(())
    }

    fn apply_crypto(&mut self, socket: SRTSOCKET, _crypto: &SrtCryptoConfig) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Crypto(socket));
        if self.fail_step == Some(FailStep::Crypto) {
            return Err("crypto failed".to_string());
        }
        Ok(())
    }

    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String> {
        self.events
            .borrow_mut()
            .push(Event::StreamId(socket, stream_id.to_string()));
        if self.fail_step == Some(FailStep::StreamId) {
            return Err("stream id failed".to_string());
        }
        Ok(())
    }

    fn bind_muxer_port(&mut self, socket: SRTSOCKET, port: u16) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Bind(socket, port));
        if self.fail_step == Some(FailStep::Bind) {
            return Err("bind failed".to_string());
        }
        Ok(())
    }

    fn connect(&mut self, socket: SRTSOCKET, peer_addr: SocketAddr) -> Result<(), String> {
        self.events
            .borrow_mut()
            .push(Event::Connect(socket, peer_addr));
        if self.fail_step == Some(FailStep::Connect) {
            return Err("connect failed".to_string());
        }
        Ok(())
    }

    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        self.events
            .borrow_mut()
            .push(Event::Configure(socket, mode));
        if self.fail_step == Some(FailStep::Configure) {
            return Err(SrtEgressSocketError {
                option: "SRTO_SNDSYN",
                code: 1234,
                message: "configure failed".to_string(),
            });
        }
        Ok(())
    }

    fn connected_local_port(&mut self, socket: SRTSOCKET) -> Result<u16, String> {
        self.events.borrow_mut().push(Event::LocalPort(socket));
        Ok(self.local_port)
    }

    fn log_effective_opts(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Log(socket));
    }
}
