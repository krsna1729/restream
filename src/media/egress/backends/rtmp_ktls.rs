//! Linux kTLS handoff for an already-completed Rustls client handshake.
//!
//! Linux exposes the TLS 1.2 AES-GCM record format through `SOL_TLS`. Rustls
//! supplies the negotiated traffic keys and record sequence numbers; after
//! this handoff ordinary `read`/`write` syscalls use kernel TLS records and no
//! userspace ciphertext buffer remains.

use std::io;
use std::os::fd::RawFd;

use tokio_rustls::rustls::{
    CipherSuite, ConnectionTrafficSecrets, ExtractedSecrets, ProtocolVersion,
};

const SOL_TLS: libc::c_int = 0x11a;
const TLS_TX: libc::c_int = 1;
const TLS_RX: libc::c_int = 2;
const TCP_ULP: libc::c_int = 31;
const TLS_1_2: u16 = 0x0303;
const TLS_CIPHER_AES_GCM_128: u16 = 51;
const TLS_CIPHER_AES_GCM_256: u16 = 52;

#[repr(C)]
#[derive(Clone, Copy)]
struct TlsCryptoInfo {
    version: u16,
    cipher_type: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Tls12AesGcm128 {
    info: TlsCryptoInfo,
    iv: [u8; 8],
    key: [u8; 16],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Tls12AesGcm256 {
    info: TlsCryptoInfo,
    iv: [u8; 8],
    key: [u8; 32],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

pub(crate) fn available() -> bool {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return false;
    }
    let ulp = b"tls\0";
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            TCP_ULP,
            ulp.as_ptr().cast(),
            ulp.len() as libc::socklen_t,
        )
    };
    unsafe { libc::close(fd) };
    result == 0
}

pub(crate) fn install(
    fd: RawFd,
    version: ProtocolVersion,
    suite: CipherSuite,
    secrets: &ExtractedSecrets,
) -> io::Result<()> {
    if version != ProtocolVersion::TLSv1_2 {
        return Err(unsupported("Linux kTLS baseline requires TLS 1.2"));
    }
    let cipher_type = match suite {
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256 => TLS_CIPHER_AES_GCM_128,
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384 => TLS_CIPHER_AES_GCM_256,
        _ => {
            return Err(unsupported(
                "TLS cipher is not supported by the kTLS baseline",
            ));
        }
    };

    let ulp = b"tls\0";
    set_socket_option(
        fd,
        libc::IPPROTO_TCP,
        TCP_ULP,
        ulp.as_ptr().cast(),
        ulp.len(),
    )?;
    match cipher_type {
        TLS_CIPHER_AES_GCM_128 => {
            let tx = aes128_info(cipher_type, secrets.tx.0, &secrets.tx.1)?;
            let rx = aes128_info(cipher_type, secrets.rx.0, &secrets.rx.1)?;
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_TX,
                &tx as *const Tls12AesGcm128 as *const libc::c_void,
                size_of_val(&tx),
            )?;
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_RX,
                &rx as *const Tls12AesGcm128 as *const libc::c_void,
                size_of_val(&rx),
            )?;
        }
        TLS_CIPHER_AES_GCM_256 => {
            let tx = aes256_info(cipher_type, secrets.tx.0, &secrets.tx.1)?;
            let rx = aes256_info(cipher_type, secrets.rx.0, &secrets.rx.1)?;
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_TX,
                &tx as *const Tls12AesGcm256 as *const libc::c_void,
                size_of_val(&tx),
            )?;
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_RX,
                &rx as *const Tls12AesGcm256 as *const libc::c_void,
                size_of_val(&rx),
            )?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn aes128_info(
    cipher_type: u16,
    sequence: u64,
    secret: &ConnectionTrafficSecrets,
) -> io::Result<Tls12AesGcm128> {
    let ConnectionTrafficSecrets::Aes128Gcm { key, iv } = secret else {
        return Err(unsupported(
            "Rustls secret does not match the negotiated AES-128 suite",
        ));
    };
    let mut result = Tls12AesGcm128 {
        info: TlsCryptoInfo {
            version: TLS_1_2,
            cipher_type,
        },
        iv: [0; 8],
        key: [0; 16],
        salt: [0; 4],
        rec_seq: sequence.to_be_bytes(),
    };
    let iv = iv.as_ref();
    if iv.len() != 12 || key.as_ref().len() != 16 {
        return Err(unsupported(
            "Rustls AES-128 traffic secret has an invalid shape",
        ));
    }
    result.salt.copy_from_slice(&iv[..4]);
    result.iv.copy_from_slice(&iv[4..]);
    result.key.copy_from_slice(key.as_ref());
    Ok(result)
}

fn aes256_info(
    cipher_type: u16,
    sequence: u64,
    secret: &ConnectionTrafficSecrets,
) -> io::Result<Tls12AesGcm256> {
    let ConnectionTrafficSecrets::Aes256Gcm { key, iv } = secret else {
        return Err(unsupported(
            "Rustls secret does not match the negotiated AES-256 suite",
        ));
    };
    let mut result = Tls12AesGcm256 {
        info: TlsCryptoInfo {
            version: TLS_1_2,
            cipher_type,
        },
        iv: [0; 8],
        key: [0; 32],
        salt: [0; 4],
        rec_seq: sequence.to_be_bytes(),
    };
    let iv = iv.as_ref();
    if iv.len() != 12 || key.as_ref().len() != 32 {
        return Err(unsupported(
            "Rustls AES-256 traffic secret has an invalid shape",
        ));
    }
    result.salt.copy_from_slice(&iv[..4]);
    result.iv.copy_from_slice(&iv[4..]);
    result.key.copy_from_slice(key.as_ref());
    Ok(result)
}

fn set_socket_option(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    value: *const libc::c_void,
    length: usize,
) -> io::Result<()> {
    let result = unsafe { libc::setsockopt(fd, level, name, value, length as libc::socklen_t) };
    (result == 0)
        .then_some(())
        .ok_or_else(io::Error::last_os_error)
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_probe_is_safe_on_linux() {
        let _ = available();
    }

    #[test]
    fn non_tls12_handoff_is_rejected_before_touching_the_socket() {
        let error = install(
            -1,
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_AES_128_GCM_SHA256,
            &ExtractedSecrets {
                tx: (
                    0,
                    ConnectionTrafficSecrets::Aes128Gcm {
                        key: [0; 32].into(),
                        iv: [0; 12].into(),
                    },
                ),
                rx: (
                    0,
                    ConnectionTrafficSecrets::Aes128Gcm {
                        key: [0; 32].into(),
                        iv: [0; 12].into(),
                    },
                ),
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
