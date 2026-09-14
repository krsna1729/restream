//! Linux kTLS handoff for an already-completed Rustls client handshake.
//!
//! Linux exposes the TLS 1.2 AES-GCM record format through `SOL_TLS`. Rustls
//! supplies the negotiated traffic keys and record sequence numbers; after
//! this handoff ordinary `read`/`write` syscalls use kernel TLS records and no
//! userspace ciphertext buffer remains.

use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::{LazyLock, Mutex};

use tokio_rustls::rustls::{
    CipherSuite, ConnectionTrafficSecrets, ExtractedSecrets, ProtocolVersion,
};

const SOL_TLS: libc::c_int = 0x11a;
const TLS_TX: libc::c_int = 1;
const TLS_RX: libc::c_int = 2;
const TCP_ULP: libc::c_int = 31;
const TLS_1_2: u16 = 0x0303;
const TLS_1_3: u16 = 0x0304;
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

/// Exact `(TLS version, AES-GCM cipher)` combinations the running kernel has
/// proven it accepts through `SOL_TLS`. `available()` only proves
/// `TCP_ULP="tls"` exists; it says nothing about TLS 1.3 or about a
/// particular cipher. Probing the exact matrix *before* the caller consumes
/// its Rustls connection via secret extraction keeps an unsupported kTLS
/// attempt a userspace-TLS session instead of a failed output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum KtlsCipher {
    Aes128Gcm,
    Aes256Gcm,
}

static KTLS_CAPABILITY_CACHE: LazyLock<Mutex<HashMap<(bool, KtlsCipher), bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns `true` only if the running kernel has already proven it accepts
/// this exact `(version, cipher)` for both `TLS_TX` and `TLS_RX`. The first
/// call for a combination runs a once-per-process probe against a connected
/// local TCP pair with dummy key material; later calls reuse the cached
/// verdict without touching the kernel.
pub(crate) fn supports(version: ProtocolVersion, suite: CipherSuite) -> bool {
    let Some((tls13, cipher)) = capability_for(version, suite) else {
        return false;
    };
    let key = (tls13, cipher);
    if let Ok(cache) = KTLS_CAPABILITY_CACHE.lock()
        && let Some(&proven) = cache.get(&key)
    {
        return proven;
    }
    let proven = probe_capability(version, suite, cipher);
    if let Ok(mut cache) = KTLS_CAPABILITY_CACHE.lock() {
        cache.insert(key, proven);
    }
    proven
}

fn capability_for(version: ProtocolVersion, suite: CipherSuite) -> Option<(bool, KtlsCipher)> {
    let tls13 = match version {
        ProtocolVersion::TLSv1_2 => false,
        ProtocolVersion::TLSv1_3 => true,
        _ => return None,
    };
    let cipher = match suite {
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS13_AES_128_GCM_SHA256 => KtlsCipher::Aes128Gcm,
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS13_AES_256_GCM_SHA384 => KtlsCipher::Aes256Gcm,
        _ => return None,
    };
    Some((tls13, cipher))
}

fn probe_capability(version: ProtocolVersion, suite: CipherSuite, cipher: KtlsCipher) -> bool {
    let wire_version = match version {
        ProtocolVersion::TLSv1_2 => TLS_1_2,
        ProtocolVersion::TLSv1_3 => TLS_1_3,
        _ => return false,
    };
    let cipher_type = match cipher {
        KtlsCipher::Aes128Gcm => TLS_CIPHER_AES_GCM_128,
        KtlsCipher::Aes256Gcm => TLS_CIPHER_AES_GCM_256,
    };
    // Cipher-suite allowlist must agree with `install`, otherwise the probe
    // would claim a combination `install` itself rejects.
    if install_capability_mismatch(version, suite, cipher) {
        return false;
    }
    // A connected local TCP pair is enough: kTLS only needs a TCP socket
    // for `TCP_ULP` + `SOL_TLS`, never a real TLS session. Dummy key
    // material proves whether the kernel accepts the exact
    // `(version, cipher)` crypto-info shape for both directions.
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(_) => return false,
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(_) => return false,
    };
    let connector = match std::net::TcpStream::connect(addr) {
        Ok(connector) => connector,
        Err(_) => return false,
    };
    let (accepted, _) = match listener.accept() {
        Ok(accepted) => accepted,
        Err(_) => return false,
    };
    let _ = connector;
    use std::os::unix::io::AsRawFd;
    let fd = accepted.as_raw_fd();
    let ulp = b"tls\0";
    if set_socket_option(
        fd,
        libc::IPPROTO_TCP,
        TCP_ULP,
        ulp.as_ptr().cast(),
        ulp.len(),
    )
    .is_err()
    {
        return false;
    }
    match cipher {
        KtlsCipher::Aes128Gcm => {
            let info = Tls12AesGcm128 {
                info: TlsCryptoInfo {
                    version: wire_version,
                    cipher_type,
                },
                iv: [0xa5; 8],
                key: [0x5a; 16],
                salt: [0xa5; 4],
                rec_seq: [0; 8],
            };
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_TX,
                &info as *const Tls12AesGcm128 as *const libc::c_void,
                size_of_val(&info),
            )
            .and(set_socket_option(
                fd,
                SOL_TLS,
                TLS_RX,
                &info as *const Tls12AesGcm128 as *const libc::c_void,
                size_of_val(&info),
            ))
            .is_ok()
        }
        KtlsCipher::Aes256Gcm => {
            let info = Tls12AesGcm256 {
                info: TlsCryptoInfo {
                    version: wire_version,
                    cipher_type,
                },
                iv: [0xa5; 8],
                key: [0x5a; 32],
                salt: [0xa5; 4],
                rec_seq: [0; 8],
            };
            set_socket_option(
                fd,
                SOL_TLS,
                TLS_TX,
                &info as *const Tls12AesGcm256 as *const libc::c_void,
                size_of_val(&info),
            )
            .and(set_socket_option(
                fd,
                SOL_TLS,
                TLS_RX,
                &info as *const Tls12AesGcm256 as *const libc::c_void,
                size_of_val(&info),
            ))
            .is_ok()
        }
    }
}

fn install_capability_mismatch(
    version: ProtocolVersion,
    suite: CipherSuite,
    cipher: KtlsCipher,
) -> bool {
    let expected = match suite {
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS13_AES_128_GCM_SHA256 => KtlsCipher::Aes128Gcm,
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS13_AES_256_GCM_SHA384 => KtlsCipher::Aes256Gcm,
        _ => return true,
    };
    if expected != cipher {
        return true;
    }
    !matches!(version, ProtocolVersion::TLSv1_2 | ProtocolVersion::TLSv1_3)
}

pub(crate) fn install(
    fd: RawFd,
    version: ProtocolVersion,
    suite: CipherSuite,
    secrets: &ExtractedSecrets,
) -> io::Result<()> {
    let wire_version = match version {
        ProtocolVersion::TLSv1_2 => TLS_1_2,
        ProtocolVersion::TLSv1_3 => TLS_1_3,
        _ => return Err(unsupported("Linux kTLS requires TLS 1.2 or TLS 1.3")),
    };
    let cipher_type = match suite {
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
        | CipherSuite::TLS13_AES_128_GCM_SHA256 => TLS_CIPHER_AES_GCM_128,
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
        | CipherSuite::TLS13_AES_256_GCM_SHA384 => TLS_CIPHER_AES_GCM_256,
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
            let tx = aes128_info(wire_version, cipher_type, secrets.tx.0, &secrets.tx.1)?;
            let rx = aes128_info(wire_version, cipher_type, secrets.rx.0, &secrets.rx.1)?;
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
            let tx = aes256_info(wire_version, cipher_type, secrets.tx.0, &secrets.tx.1)?;
            let rx = aes256_info(wire_version, cipher_type, secrets.rx.0, &secrets.rx.1)?;
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
    version: u16,
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
            version,
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
    version: u16,
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
            version,
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
        let _ = supports(
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_AES_128_GCM_SHA256,
        );
        let _ = supports(
            ProtocolVersion::TLSv1_2,
            CipherSuite::TLS13_AES_128_GCM_SHA256,
        );
    }

    #[test]
    fn unknown_cipher_never_probes_the_kernel() {
        assert!(!supports(
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        ));
    }

    #[test]
    fn unsupported_cipher_is_rejected_before_touching_the_socket() {
        let error = install(
            -1,
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
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

    #[test]
    fn tls13_aes_gcm_uses_the_tls13_crypto_info_version() {
        let secret = ConnectionTrafficSecrets::Aes256Gcm {
            key: [0; 32].into(),
            iv: [0; 12].into(),
        };
        let info = aes256_info(TLS_1_3, TLS_CIPHER_AES_GCM_256, 0, &secret).unwrap();
        assert_eq!(info.info.version, TLS_1_3);
    }
}
