//! Origin viewer tokens: `v1.<pipeline>.<expiry>.<mac>`.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub const TOKEN_TTL: Duration = Duration::from_secs(60);
const VERSION: &str = "v1";
const CONTEXT: &[u8] = b"restream-moq-origin-token\0";
const BLOCK: usize = 64;

pub type TokenKey = [u8; 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    Forged,
    Expired,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed MoQ viewer token",
            Self::Forged => "MoQ viewer token signature mismatch",
            Self::Expired => "MoQ viewer token expired",
        })
    }
}

impl std::error::Error for TokenError {}

pub fn process_key() -> &'static TokenKey {
    static KEY: LazyLock<TokenKey> = LazyLock::new(rand::random);
    &KEY
}

pub fn mint(key: &TokenKey, pipeline_id: &str, now_secs: u64) -> (String, u64) {
    let expiry = now_secs.saturating_add(TOKEN_TTL.as_secs());
    let mac = mac(key, pipeline_id, expiry);
    let token = format!(
        "{VERSION}.{}.{expiry}.{}",
        URL_SAFE_NO_PAD.encode(pipeline_id),
        URL_SAFE_NO_PAD.encode(mac)
    );
    (token, expiry)
}

pub fn verify(key: &TokenKey, token: &str, now_secs: u64) -> Result<String, TokenError> {
    let mut parts = token.split('.');
    let (Some(VERSION), Some(pipeline), Some(expiry), Some(mac), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(TokenError::Malformed);
    };
    let pipeline = URL_SAFE_NO_PAD
        .decode(pipeline)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(TokenError::Malformed)?;
    let expiry: u64 = expiry.parse().map_err(|_| TokenError::Malformed)?;
    let presented: [u8; 32] = URL_SAFE_NO_PAD
        .decode(mac)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(TokenError::Malformed)?;
    if !constant_time_eq(&presented, &self::mac(key, &pipeline, expiry)) {
        return Err(TokenError::Forged);
    }
    if expiry > now_secs.saturating_add(TOKEN_TTL.as_secs()) {
        return Err(TokenError::Forged);
    }
    if now_secs > expiry {
        return Err(TokenError::Expired);
    }
    Ok(pipeline)
}

fn mac(key: &TokenKey, pipeline_id: &str, expiry: u64) -> [u8; 32] {
    hmac_sha256(
        key,
        &[
            CONTEXT,
            &(pipeline_id.len() as u64).to_be_bytes(),
            pipeline_id.as_bytes(),
            &expiry.to_be_bytes(),
        ],
    )
}

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    for part in parts {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let diff = left
        .iter()
        .zip(right)
        .fold(0u8, |diff, (left, right)| diff | (left ^ right));
    std::hint::black_box(diff) == 0
}
