//! Domain model for SRT ingest security policy and latency resolution.

use serde::{Deserialize, Serialize};

pub const DEFAULT_SRT_PBKEYLEN: i32 = 16;

/// Historical flat listener default (`media::srt::socket::DESIRED_LATENCY_MS`),
/// duplicated here rather than imported: `domain` sits below `media` in the
/// layering and must not depend on it. This is a stable, protocol-adjacent
/// constant unlikely to drift, but if `DESIRED_LATENCY_MS` ever changes,
/// change this to match.
pub const DEFAULT_SRT_INGEST_LATENCY_MS: i32 = 250;

/// The SRT wire protocol's own documented range for the negotiated TSBPD
/// delay field (`TsbPdDelay`/`RcvTsbPdDelay`/`SndTsbPdDelay` in the vendored
/// libsrt handshake spec, `docs/features/handshake.md`) — not a value this
/// repo invented. Mirrors `media::srt::buffer_sizing::SRT_LATENCY_MS_FLOOR`;
/// duplicated for the same layering reason as the constant above.
pub const SRT_INGEST_LATENCY_MS_FLOOR: i32 = 20;
pub const SRT_INGEST_LATENCY_MS_CEILING: i32 = 8_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SrtGlobalIngestMode {
    Plaintext,
    Encrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SrtGlobalIngestConfig {
    pub mode: SrtGlobalIngestMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(default = "default_srt_pbkeylen")]
    pub pbkeylen: i32,
    /// This caller's own proposed minimum TSBPD delay (`SRTO_RCVLATENCY`)
    /// for every ingest connection that doesn't set a per-pipeline
    /// override. See `SrtPipelineIngestConfig::latency_ms` for why this is
    /// the only latency lever ingest can offer at all (the actually
    /// negotiated value is `max(this, the caller's own PEERLATENCY)`, and
    /// PREBIND options like `SRTO_RCVBUF` must be fixed before that
    /// negotiation completes — see `media::srt::listener`).
    #[serde(default = "default_srt_ingest_latency_ms")]
    pub latency_ms: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SrtPipelineIngestMode {
    Inherit,
    Plaintext,
    Encrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SrtPipelineIngestConfig {
    pub mode: SrtPipelineIngestMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbkeylen: Option<i32>,
    /// `None` inherits `SrtGlobalIngestConfig::latency_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i32>,
}

/// Ingest's own receive-side buffer-sizing formula (`media::srt::
/// buffer_sizing`) also derives `SRTO_RCVBUF`/`SRTO_FC` from this value —
/// see that module for the "why", and why it can only ever be sized from
/// this configured value, never the value actually negotiated with a
/// caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSrtCrypto {
    Plaintext,
    Encrypted { passphrase: String, pbkeylen: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSrtIngestConfig {
    pub crypto: ResolvedSrtCrypto,
    pub latency_ms: i32,
}

fn default_srt_pbkeylen() -> i32 {
    DEFAULT_SRT_PBKEYLEN
}

fn default_srt_ingest_latency_ms() -> i32 {
    DEFAULT_SRT_INGEST_LATENCY_MS
}

impl Default for SrtGlobalIngestConfig {
    fn default() -> Self {
        Self {
            mode: SrtGlobalIngestMode::Plaintext,
            passphrase: None,
            pbkeylen: DEFAULT_SRT_PBKEYLEN,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        }
    }
}

impl Default for SrtPipelineIngestConfig {
    fn default() -> Self {
        Self {
            mode: SrtPipelineIngestMode::Inherit,
            passphrase: None,
            pbkeylen: None,
            latency_ms: None,
        }
    }
}

impl SrtGlobalIngestConfig {
    pub fn validate(&mut self) -> Result<(), String> {
        self.pbkeylen = normalize_srt_pbkeylen(self.pbkeylen)?;
        self.latency_ms = normalize_srt_ingest_latency_ms(self.latency_ms)?;
        match self.mode {
            SrtGlobalIngestMode::Plaintext => {
                self.passphrase = None;
            }
            SrtGlobalIngestMode::Encrypted => {
                let passphrase = self
                    .passphrase
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| "SRT encrypted mode requires a passphrase".to_string())?;
                validate_srt_passphrase(passphrase)?;
                self.passphrase = Some(passphrase.to_string());
            }
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<ResolvedSrtIngestConfig, String> {
        let crypto = match self.mode {
            SrtGlobalIngestMode::Plaintext => ResolvedSrtCrypto::Plaintext,
            SrtGlobalIngestMode::Encrypted => {
                let passphrase = self
                    .passphrase
                    .clone()
                    .ok_or_else(|| "missing SRT passphrase".to_string())?;
                validate_srt_passphrase(&passphrase)?;
                ResolvedSrtCrypto::Encrypted {
                    passphrase,
                    pbkeylen: normalize_srt_pbkeylen(self.pbkeylen)?,
                }
            }
        };
        Ok(ResolvedSrtIngestConfig {
            crypto,
            latency_ms: normalize_srt_ingest_latency_ms(self.latency_ms)?,
        })
    }
}

impl SrtPipelineIngestConfig {
    pub fn validate(&mut self) -> Result<(), String> {
        match self.mode {
            SrtPipelineIngestMode::Inherit | SrtPipelineIngestMode::Plaintext => {
                self.passphrase = None;
                self.pbkeylen = None;
            }
            SrtPipelineIngestMode::Encrypted => {
                let passphrase = self
                    .passphrase
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        "Per-pipeline encrypted SRT ingest requires a passphrase".to_string()
                    })?;
                validate_srt_passphrase(passphrase)?;
                self.passphrase = Some(passphrase.to_string());
                self.pbkeylen = Some(normalize_srt_pbkeylen(
                    self.pbkeylen.unwrap_or(DEFAULT_SRT_PBKEYLEN),
                )?);
            }
        }
        if let Some(latency_ms) = self.latency_ms {
            self.latency_ms = Some(normalize_srt_ingest_latency_ms(latency_ms)?);
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        global: &SrtGlobalIngestConfig,
    ) -> Result<ResolvedSrtIngestConfig, String> {
        let crypto = match self.mode {
            SrtPipelineIngestMode::Inherit => return self.resolve_latency_only(global),
            SrtPipelineIngestMode::Plaintext => ResolvedSrtCrypto::Plaintext,
            SrtPipelineIngestMode::Encrypted => {
                let passphrase = self
                    .passphrase
                    .clone()
                    .ok_or_else(|| "missing per-pipeline SRT passphrase".to_string())?;
                validate_srt_passphrase(&passphrase)?;
                ResolvedSrtCrypto::Encrypted {
                    passphrase,
                    pbkeylen: normalize_srt_pbkeylen(
                        self.pbkeylen.unwrap_or(DEFAULT_SRT_PBKEYLEN),
                    )?,
                }
            }
        };
        Ok(ResolvedSrtIngestConfig {
            crypto,
            latency_ms: self.resolve_latency_ms(global)?,
        })
    }

    /// `Inherit` mode resolves crypto from `global.resolve()` wholesale
    /// (preserving that function's own error/fail-closed behavior) but
    /// latency independently, since a pipeline can override latency without
    /// overriding crypto mode.
    fn resolve_latency_only(
        &self,
        global: &SrtGlobalIngestConfig,
    ) -> Result<ResolvedSrtIngestConfig, String> {
        let mut resolved = global.resolve()?;
        resolved.latency_ms = self.resolve_latency_ms(global)?;
        Ok(resolved)
    }

    fn resolve_latency_ms(&self, global: &SrtGlobalIngestConfig) -> Result<i32, String> {
        match self.latency_ms {
            Some(latency_ms) => normalize_srt_ingest_latency_ms(latency_ms),
            None => normalize_srt_ingest_latency_ms(global.latency_ms),
        }
    }
}

fn validate_srt_passphrase(passphrase: &str) -> Result<(), String> {
    let len = passphrase.len();
    if !(10..=79).contains(&len) {
        return Err("SRT passphrase must be 10-79 bytes".to_string());
    }
    Ok(())
}

fn normalize_srt_pbkeylen(pbkeylen: i32) -> Result<i32, String> {
    match pbkeylen {
        16 | 24 | 32 => Ok(pbkeylen),
        _ => Err("SRT pbkeylen must be 16, 24, or 32".to_string()),
    }
}

fn normalize_srt_ingest_latency_ms(latency_ms: i32) -> Result<i32, String> {
    if (SRT_INGEST_LATENCY_MS_FLOOR..=SRT_INGEST_LATENCY_MS_CEILING).contains(&latency_ms) {
        Ok(latency_ms)
    } else {
        Err(format!(
            "SRT ingest latency must be {SRT_INGEST_LATENCY_MS_FLOOR}-{SRT_INGEST_LATENCY_MS_CEILING}ms"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_srt_ingest_plaintext_clears_secret() {
        let mut cfg = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Plaintext,
            passphrase: Some("0123456789".to_string()),
            pbkeylen: 24,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        cfg.validate().unwrap();
        assert_eq!(cfg.passphrase, None);
        assert_eq!(
            cfg.resolve().unwrap(),
            ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            }
        );
    }

    #[test]
    fn encrypted_pipeline_policy_overrides_global() {
        let mut global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 16,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        global.validate().unwrap();

        let mut pipeline = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("pipeline-pass-123".to_string()),
            pbkeylen: Some(32),
            latency_ms: None,
        };
        pipeline.validate().unwrap();

        assert_eq!(
            pipeline.resolve(&global).unwrap(),
            ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Encrypted {
                    passphrase: "pipeline-pass-123".to_string(),
                    pbkeylen: 32,
                },
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            }
        );
    }

    #[test]
    fn inherit_pipeline_policy_uses_global() {
        let mut global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 24,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        global.validate().unwrap();
        let mut pipeline = SrtPipelineIngestConfig::default();
        pipeline.validate().unwrap();

        assert_eq!(
            pipeline.resolve(&global).unwrap(),
            ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Encrypted {
                    passphrase: "global-pass-123".to_string(),
                    pbkeylen: 24,
                },
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            }
        );
    }

    #[test]
    fn encrypted_global_resolve_rejects_malformed_secret() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: 16,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };

        assert_eq!(
            global.resolve().unwrap_err(),
            "SRT passphrase must be 10-79 bytes"
        );
    }

    #[test]
    fn encrypted_pipeline_resolve_rejects_malformed_secret() {
        let global = SrtGlobalIngestConfig::default();
        let pipeline = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: Some(16),
            latency_ms: None,
        };

        assert_eq!(
            pipeline.resolve(&global).unwrap_err(),
            "SRT passphrase must be 10-79 bytes"
        );
    }

    #[test]
    fn pipeline_latency_override_wins_regardless_of_crypto_mode() {
        let global = SrtGlobalIngestConfig::default();
        let mut pipeline = SrtPipelineIngestConfig {
            latency_ms: Some(2_000),
            ..SrtPipelineIngestConfig::default()
        };
        pipeline.validate().unwrap();

        assert_eq!(
            pipeline.resolve(&global).unwrap(),
            ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: 2_000,
            }
        );
    }

    #[test]
    fn pipeline_without_latency_override_inherits_global_latency() {
        let global = SrtGlobalIngestConfig {
            latency_ms: 400,
            ..SrtGlobalIngestConfig::default()
        };
        let pipeline = SrtPipelineIngestConfig::default();

        assert_eq!(pipeline.resolve(&global).unwrap().latency_ms, 400);
    }

    #[test]
    fn global_validate_rejects_out_of_range_latency() {
        let mut global = SrtGlobalIngestConfig {
            latency_ms: 10,
            ..SrtGlobalIngestConfig::default()
        };
        assert_eq!(
            global.validate().unwrap_err(),
            "SRT ingest latency must be 20-8000ms"
        );

        let mut global = SrtGlobalIngestConfig {
            latency_ms: 8_001,
            ..SrtGlobalIngestConfig::default()
        };
        assert_eq!(
            global.validate().unwrap_err(),
            "SRT ingest latency must be 20-8000ms"
        );
    }

    #[test]
    fn pipeline_validate_rejects_out_of_range_latency_override() {
        let mut pipeline = SrtPipelineIngestConfig {
            latency_ms: Some(9_000),
            ..SrtPipelineIngestConfig::default()
        };
        assert_eq!(
            pipeline.validate().unwrap_err(),
            "SRT ingest latency must be 20-8000ms"
        );
    }

    #[test]
    fn latency_bounds_are_inclusive() {
        let mut global = SrtGlobalIngestConfig {
            latency_ms: SRT_INGEST_LATENCY_MS_FLOOR,
            ..SrtGlobalIngestConfig::default()
        };
        global.validate().unwrap();
        assert_eq!(global.latency_ms, SRT_INGEST_LATENCY_MS_FLOOR);

        let mut global = SrtGlobalIngestConfig {
            latency_ms: SRT_INGEST_LATENCY_MS_CEILING,
            ..SrtGlobalIngestConfig::default()
        };
        global.validate().unwrap();
        assert_eq!(global.latency_ms, SRT_INGEST_LATENCY_MS_CEILING);
    }
}
