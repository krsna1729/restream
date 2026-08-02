//! Shared RTMP client and server handshake state machines.

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
#[cfg(test)]
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

pub(super) async fn perform_server_handshake(
    socket: &mut TcpStream,
    buffer: &mut [u8],
) -> Result<Vec<u8>, &'static str> {
    let mut handshake = Handshake::new(PeerType::Server);

    loop {
        let n = socket
            .read(buffer)
            .await
            .map_err(|_| "Socket read error during handshake")?;
        if n == 0 {
            return Err("Socket closed during handshake");
        }

        let result = handshake
            .process_bytes(&buffer[..n])
            .map_err(|_| "Handshake parsing error")?;
        match result {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    socket
                        .write_all(&response_bytes)
                        .await
                        .map_err(|_| "Socket write error during handshake")?;
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    socket
                        .write_all(&response_bytes)
                        .await
                        .map_err(|_| "Socket write error during handshake")?;
                }
                return Ok(remaining_bytes);
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn perform_client_handshake<S>(
    socket: &mut S,
    cancel_token: &CancellationToken,
) -> Result<Vec<u8>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake = Handshake::new(PeerType::Client);
    let c0_c1 = handshake
        .generate_outbound_p0_and_p1()
        .map_err(|e| format!("{e:?}"))?;

    socket
        .write_all(&c0_c1)
        .await
        .map_err(|_| "failed to write handshake".to_string())?;

    let mut buffer = vec![0u8; 4096];
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => return Err("cancelled during handshake".to_string()),
            res = socket.read(&mut buffer) => {
                let n = match res {
                    Ok(n) if n > 0 => n,
                    _ => return Err("remote closed during handshake".to_string()),
                };
                match handshake.process_bytes(&buffer[..n]) {
                    Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                        if !response_bytes.is_empty() {
                            socket
                                .write_all(&response_bytes)
                                .await
                                .map_err(|_| "failed to write handshake response".to_string())?;
                        }
                    }
                    Ok(HandshakeProcessResult::Completed {
                        response_bytes,
                        remaining_bytes,
                    }) => {
                        if !response_bytes.is_empty() {
                            socket
                                .write_all(&response_bytes)
                                .await
                                .map_err(|_| "failed to write handshake completion".to_string())?;
                        }
                        return Ok(remaining_bytes);
                    }
                    Err(e) => return Err(format!("{e:?}")),
                }
            }
        }
    }
}
