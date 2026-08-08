use std::ffi::CString;
use std::os::raw::{c_int, c_void};

use crate::domain::srt_ingest::ResolvedSrtCrypto;

use super::{
    SRTO_ENFORCEDENCRYPTION, SRTO_PASSPHRASE, SRTO_PBKEYLEN, SRTSOCKET, check_srt_option_result,
    srt_setsockopt,
};

#[derive(Clone)]
pub(in crate::media::srt) struct SrtCryptoConfig {
    passphrase: String,
    pub(super) pbkeylen: c_int,
}

pub(super) fn srt_crypto_from_resolved(crypto: ResolvedSrtCrypto) -> Option<SrtCryptoConfig> {
    match crypto {
        ResolvedSrtCrypto::Plaintext => None,
        ResolvedSrtCrypto::Encrypted {
            passphrase,
            pbkeylen,
        } => Some(SrtCryptoConfig {
            passphrase,
            pbkeylen,
        }),
    }
}

pub(super) fn srt_crypto_from_url(
    passphrase: String,
    pbkeylen: Option<c_int>,
) -> Option<SrtCryptoConfig> {
    (!passphrase.is_empty()).then_some(SrtCryptoConfig {
        passphrase,
        pbkeylen: pbkeylen.unwrap_or(16),
    })
}

pub(super) fn apply_srt_crypto_socket(
    sock: SRTSOCKET,
    crypto: &SrtCryptoConfig,
) -> Result<(), String> {
    let passphrase =
        CString::new(crypto.passphrase.as_str()).map_err(|_| "invalid SRT passphrase")?;
    let enforced: c_int = 1;
    let pbkeylen = crypto.pbkeylen;
    unsafe {
        check_srt_option_result(
            "SRTO_PASSPHRASE",
            srt_setsockopt(
                sock,
                0,
                SRTO_PASSPHRASE,
                passphrase.as_ptr() as *const c_void,
                crypto.passphrase.len() as c_int,
            ),
        )?;
        check_srt_option_result(
            "SRTO_PBKEYLEN",
            srt_setsockopt(
                sock,
                0,
                SRTO_PBKEYLEN,
                &pbkeylen as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            ),
        )?;
        check_srt_option_result(
            "SRTO_ENFORCEDENCRYPTION",
            srt_setsockopt(
                sock,
                0,
                SRTO_ENFORCEDENCRYPTION,
                &enforced as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            ),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_resolved_config_yields_no_crypto() {
        assert!(srt_crypto_from_resolved(ResolvedSrtCrypto::Plaintext).is_none());
    }

    #[test]
    fn encrypted_resolved_config_carries_exact_passphrase_and_pbkeylen() {
        let crypto = srt_crypto_from_resolved(ResolvedSrtCrypto::Encrypted {
            passphrase: "correct-horse-battery-staple".to_string(),
            pbkeylen: 24,
        })
        .expect("encrypted config must yield a crypto config");
        assert_eq!(crypto.passphrase, "correct-horse-battery-staple");
        assert_eq!(crypto.pbkeylen, 24);
    }

    #[test]
    fn url_crypto_is_none_for_empty_passphrase_even_with_explicit_pbkeylen() {
        assert!(srt_crypto_from_url(String::new(), Some(32)).is_none());
        assert!(srt_crypto_from_url(String::new(), None).is_none());
    }

    #[test]
    fn url_crypto_defaults_pbkeylen_to_sixteen_when_unspecified() {
        let crypto = srt_crypto_from_url("s3cret".to_string(), None)
            .expect("non-empty passphrase must yield a crypto config");
        assert_eq!(crypto.passphrase, "s3cret");
        assert_eq!(crypto.pbkeylen, 16);
    }

    #[test]
    fn url_crypto_passes_explicit_pbkeylen_through_unvalidated() {
        let crypto = srt_crypto_from_url("s3cret".to_string(), Some(24))
            .expect("non-empty passphrase must yield a crypto config");
        assert_eq!(crypto.pbkeylen, 24);

        // The URL boundary intentionally performs no range validation; an
        // out-of-range value is passed through so the FFI layer is the
        // single source of truth for rejecting it (see
        // `linked_libsrt_rejects_out_of_range_pbkeylen` in srt_tests.rs).
        let out_of_range = srt_crypto_from_url("s3cret".to_string(), Some(999))
            .expect("non-empty passphrase must yield a crypto config");
        assert_eq!(out_of_range.pbkeylen, 999);
    }

    #[test]
    fn socket_crypto_rejects_interior_nul_passphrase_before_touching_ffi() {
        let crypto = SrtCryptoConfig {
            passphrase: "bad\0passphrase".to_string(),
            pbkeylen: 16,
        };
        // An invalid/sentinel socket descriptor is safe here only because
        // `CString::new` must fail first, before `sock` is ever passed to
        // libsrt.
        let result = apply_srt_crypto_socket(-1, &crypto);
        assert_eq!(result, Err("invalid SRT passphrase".to_string()));
    }
}
