//! RTMP client and server handshake state machines.

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use std::future::Future;

pub trait RtmpSocket {
    fn read(&mut self, buffer: Vec<u8>) -> impl Future<Output = std::io::Result<(usize, Vec<u8>)>>;
    fn write_all<'a>(&'a mut self, bytes: &'a [u8]) -> impl Future<Output = std::io::Result<()>>;
}

pub async fn perform_server_handshake<S: RtmpSocket>(
    socket: &mut S,
    mut buffer: Vec<u8>,
) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let mut handshake = Handshake::new(PeerType::Server);
    loop {
        let (count, returned) = socket
            .read(buffer)
            .await
            .map_err(|_| "Socket read error during handshake")?;
        buffer = returned;
        if count == 0 {
            return Err("Socket closed during handshake");
        }
        let Some(received) = buffer.get(..count) else {
            return Err("Socket read reported more bytes than its buffer");
        };
        let result = handshake
            .process_bytes(received)
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
                return Ok((response_bytes, remaining_bytes));
            }
        }
    }
}
