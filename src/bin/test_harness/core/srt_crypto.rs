use tokio::process::Command;

fn harness_srt_passphrase() -> Option<String> {
    std::env::var("HARNESS_SRT_PASSPHRASE")
        .ok()
        .filter(|value| !value.is_empty())
}

fn harness_srt_pbkeylen() -> Option<String> {
    std::env::var("HARNESS_SRT_PBKEYLEN")
        .ok()
        .filter(|value| !value.is_empty())
}

/// SRT encryption parameters injected into harness SRT listeners and URLs.
#[derive(Clone, Debug)]
pub(crate) struct HarnessSrtCrypto {
    pub(crate) label: String,
    pub(crate) passphrase: Option<String>,
    pub(crate) pbkeylen: Option<String>,
}

impl HarnessSrtCrypto {
    pub(crate) fn plaintext() -> Self {
        Self {
            label: "plaintext".to_string(),
            passphrase: None,
            pbkeylen: None,
        }
    }

    pub(crate) fn encrypted(pbkeylen: u32) -> Self {
        Self {
            label: format!("encrypted-{pbkeylen}"),
            passphrase: Some("0123456789abcd".to_string()),
            pbkeylen: Some(pbkeylen.to_string()),
        }
    }

    pub(crate) fn transport_label(&self) -> String {
        match (&self.passphrase, &self.pbkeylen) {
            (None, _) => "plaintext".to_string(),
            (Some(_), Some(len)) => format!("encrypted-{len}"),
            (Some(_), None) => "encrypted".to_string(),
        }
    }
}

pub(crate) fn harness_srt_crypto_from_env() -> HarnessSrtCrypto {
    match harness_srt_passphrase() {
        Some(passphrase) => HarnessSrtCrypto {
            label: match harness_srt_pbkeylen() {
                Some(len) => format!("encrypted-{len}"),
                None => "encrypted".to_string(),
            },
            passphrase: Some(passphrase),
            pbkeylen: harness_srt_pbkeylen(),
        },
        None => HarnessSrtCrypto::plaintext(),
    }
}

pub(crate) fn parse_srt_crypto_variants(
    name: &str,
    default: &str,
) -> Result<Vec<HarnessSrtCrypto>, String> {
    let mut out = Vec::new();
    for part in std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let variant = match part.to_ascii_lowercase().as_str() {
            "plaintext" | "plain" => HarnessSrtCrypto::plaintext(),
            "encrypted-16" | "enc16" | "aes128" | "128" => HarnessSrtCrypto::encrypted(16),
            "encrypted-24" | "enc24" | "aes192" | "192" => HarnessSrtCrypto::encrypted(24),
            "encrypted-32" | "enc32" | "aes256" | "256" => HarnessSrtCrypto::encrypted(32),
            other => {
                return Err(format!(
                    "{name} contains unsupported SRT crypto variant '{other}'"
                ));
            }
        };
        if out
            .iter()
            .all(|existing: &HarnessSrtCrypto| existing.label != variant.label)
        {
            out.push(variant);
        }
    }
    if out.is_empty() {
        return Err(format!("{name} did not resolve to any SRT crypto variants"));
    }
    Ok(out)
}

pub(crate) fn append_srt_crypto(url: String, crypto: &HarnessSrtCrypto) -> String {
    let Some(passphrase) = crypto.passphrase.as_deref() else {
        return url;
    };
    let separator = if url.contains('?') { '&' } else { '?' };
    let mut out = format!("{url}{separator}passphrase={passphrase}");
    if let Some(pbkeylen) = crypto.pbkeylen.as_deref() {
        out.push_str(&format!("&pbkeylen={pbkeylen}"));
    }
    out
}

pub(crate) fn apply_srt_listener_env(cmd: &mut Command, crypto: &HarnessSrtCrypto) {
    if let Some(passphrase) = crypto.passphrase.as_deref() {
        cmd.env("RESTREAM_SRT_PASSPHRASE", passphrase);
        if let Some(pbkeylen) = crypto.pbkeylen.as_deref() {
            cmd.env("RESTREAM_SRT_PBKEYLEN", pbkeylen);
        }
    } else {
        cmd.env_remove("RESTREAM_SRT_PASSPHRASE");
        cmd.env_remove("RESTREAM_SRT_PBKEYLEN");
    }
}

pub(crate) fn apply_harness_srt_listener_env(cmd: &mut Command) {
    apply_srt_listener_env(cmd, &harness_srt_crypto_from_env());
}
