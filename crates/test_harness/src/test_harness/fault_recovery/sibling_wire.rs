//! Real RTMP lifecycle peers: no FFmpeg startup per log line, no packet logging.
use std::os::fd::AsRawFd;
use std::time::Duration;

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(super) async fn peer(port: u16, key: &str, publish: bool) -> Result<TcpStream, String> {
    tokio::time::timeout(Duration::from_secs(3), connect(port, key, publish))
        .await
        .map_err(|_| "RTMP lifecycle handshake exceeded 3 s".to_string())?
}

async fn connect(port: u16, key: &str, publish: bool) -> Result<TcpStream, String> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.map_err(err)?;
    socket.set_nodelay(true).map_err(err)?;
    let mut handshake = Handshake::new(PeerType::Client);
    socket
        .write_all(&handshake.generate_outbound_p0_and_p1().map_err(err)?)
        .await
        .map_err(err)?;
    let mut buffer = [0; 8192];
    let remaining = loop {
        let n = socket.read(&mut buffer).await.map_err(err)?;
        if n == 0 {
            return Err("RTMP peer closed during handshake".into());
        }
        match handshake.process_bytes(&buffer[..n]).map_err(err)? {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket.write_all(&response_bytes).await.map_err(err)?;
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket.write_all(&response_bytes).await.map_err(err)?;
                break remaining_bytes;
            }
        }
    };
    let (mut session, initial) = ClientSession::new(ClientSessionConfig::new()).map_err(err)?;
    for result in initial {
        send(&mut socket, result).await?;
    }
    send(
        &mut socket,
        session.request_connection("live".into()).map_err(err)?,
    )
    .await?;
    let mut received = remaining;
    loop {
        for result in session.handle_input(&received).map_err(err)? {
            match result {
                ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
                    let request = if publish {
                        session.request_publishing(key.to_string(), PublishRequestType::Live)
                    } else {
                        session.request_playback(key.to_string())
                    }
                    .map_err(err)?;
                    send(&mut socket, request).await?;
                }
                ClientSessionResult::RaisedEvent(ClientSessionEvent::PublishRequestAccepted)
                | ClientSessionResult::RaisedEvent(ClientSessionEvent::PlaybackRequestAccepted) => {
                    return Ok(socket);
                }
                other => send(&mut socket, other).await?,
            }
        }
        let n = socket.read(&mut buffer).await.map_err(err)?;
        if n == 0 {
            return Err("RTMP lifecycle peer closed before acceptance".into());
        }
        received.clear();
        received.extend_from_slice(&buffer[..n]);
    }
}

async fn send(socket: &mut TcpStream, result: ClientSessionResult) -> Result<(), String> {
    if let ClientSessionResult::OutboundResponse(packet) = result {
        socket.write_all(&packet.bytes).await.map_err(err)?;
    }
    Ok(())
}

fn err(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

pub(super) async fn malformed(port: u16, key: &str) -> Result<(), String> {
    let mut socket = peer(port, key, true).await?;
    // RTMP type-0 video message declaring 16 MiB: the production parser must
    // reject this entity rather than reserve its declaration or stop an Owner.
    let mut bytes = vec![6, 0, 0, 0, 0xff, 0xff, 0xff, 9, 1, 0, 0, 0];
    bytes.extend_from_slice(&[0; 128]);
    socket.write_all(&bytes).await.map_err(err)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut buffer = [0; 4096];
        while socket.read(&mut buffer).await.map_err(err)? > 0 {}
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| "malformed publisher not retired in 5 s".to_string())?
}

pub(super) async fn stopped_reader(port: u16, key: &str) -> Result<u64, String> {
    let socket = peer(port, key, false).await?;
    let capacity: libc::c_int = 4096;
    // SAFETY: socket owns this live fd; capacity is a valid c_int for its size.
    if unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&capacity as *const libc::c_int).cast(),
            std::mem::size_of_val(&capacity) as libc::socklen_t,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    tokio::time::sleep(Duration::from_secs(20)).await;
    let mut queued: libc::c_int = 0;
    // SAFETY: socket owns this live fd and queued is writable for one c_int.
    if unsafe { libc::ioctl(socket.as_raw_fd(), libc::FIONREAD, &mut queued) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if queued < capacity {
        return Err(format!(
            "stopped reader never filled its receive buffer: {queued} bytes"
        ));
    }
    Ok(queued as u64)
}
