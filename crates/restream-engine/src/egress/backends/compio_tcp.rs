mod poller;
mod stream;

pub(crate) use poller::CompioTcpPoller;
pub(crate) use stream::{CompioTcpStream, TxPart};

/// The result of starting a nonblocking TCP connect.
#[derive(Debug)]
pub(crate) enum TcpConnectAttempt {
    Connected(CompioTcpStream),
    InProgress(CompioTcpStream),
}
