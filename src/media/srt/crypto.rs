use std::ffi::CString;
use std::os::raw::{c_int, c_void};

use crate::domain::srt_ingest::ResolvedSrtIngestConfig;

use super::{
    SRTO_ENFORCEDENCRYPTION, SRTO_PASSPHRASE, SRTO_PBKEYLEN, SRTSOCKET, SrtSockOptConfig,
    check_srt_option_result, srt_config_add, srt_setsockopt,
};

#[derive(Clone)]
pub(super) struct SrtCryptoConfig {
    passphrase: String,
    pub(super) pbkeylen: c_int,
}

pub(super) fn srt_crypto_from_resolved(config: ResolvedSrtIngestConfig) -> Option<SrtCryptoConfig> {
    match config {
        ResolvedSrtIngestConfig::Plaintext => None,
        ResolvedSrtIngestConfig::Encrypted {
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

pub(super) unsafe fn apply_srt_crypto_config(
    config: *mut SrtSockOptConfig,
    crypto: &SrtCryptoConfig,
) -> Result<(), String> {
    let passphrase =
        CString::new(crypto.passphrase.as_str()).map_err(|_| "invalid SRT passphrase")?;
    let enforced: c_int = 1;
    unsafe {
        check_srt_option_result(
            "SRTO_PASSPHRASE",
            srt_config_add(
                config,
                SRTO_PASSPHRASE,
                passphrase.as_ptr() as *const c_void,
                crypto.passphrase.len() as c_int,
            ),
        )?;
        check_srt_option_result(
            "SRTO_PBKEYLEN",
            srt_config_add(
                config,
                SRTO_PBKEYLEN,
                &crypto.pbkeylen as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            ),
        )?;
        check_srt_option_result(
            "SRTO_ENFORCEDENCRYPTION",
            srt_config_add(
                config,
                SRTO_ENFORCEDENCRYPTION,
                &enforced as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            ),
        )?;
    }
    Ok(())
}
