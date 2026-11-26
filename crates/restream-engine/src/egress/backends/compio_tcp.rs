mod poller;
mod stream;
mod tls_connection;

pub(crate) use poller::CompioTcpPoller;
pub(crate) use stream::{CompioTcpStream, TxPart};
pub(crate) use tls_connection::TlsTcpConnection;

/// The result of starting a nonblocking TCP connect.
#[derive(Debug)]
pub(crate) enum TcpConnectAttempt {
    Connected(CompioTcpStream),
    InProgress(CompioTcpStream),
}
