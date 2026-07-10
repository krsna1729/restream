//! Centralized application configuration.
//! Reads environment variables once at startup and stores them in a typed struct.

use crate::planner::backend_policy::BackendPolicy;
use crate::{RuntimeTuning, ServerPorts};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ports: ServerPorts,
    pub http_bind_addr: String,
    pub tuning: RuntimeTuning,
    pub db_path: String,
    pub media_dir: String,
    pub log_retention_days: u64,
    pub backend_policy: BackendPolicy,
    pub rtmp_backlog: u32,
    pub ffmpeg_threads: Option<u32>,
    pub avio_capacity: usize,
    pub hls_min_segment_ms: f64,
    pub hls_segment_capacity_bytes: usize,
    pub hls_max_segments: usize,
    pub recording_threads: Option<u32>,
    pub ts_ring_capacity: usize,
    pub ring_capacity: usize,
    pub transcoder_ring_capacity: usize,
    pub require_srt_bonding: bool,
    pub external_ffmpeg_permits: usize,
    pub ffmpeg_bin_path: Option<String>,
    pub log_dir: String,
    pub no_color: bool,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub use_internal_file_ingest: bool,
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl ServerPorts {
    pub fn from_env() -> Self {
        Self {
            http: std::env::var("RESTREAM_HTTP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3030),
            rtmp: std::env::var("RESTREAM_RTMP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1935),
            srt: std::env::var("RESTREAM_SRT_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10080),
        }
    }
}

impl RuntimeTuning {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            nofile_limit: env_u64("RESTREAM_NOFILE_LIMIT", defaults.nofile_limit).max(1),
            reconciler_interval_ms: env_u64(
                "RESTREAM_RECONCILE_INTERVAL_MS",
                defaults.reconciler_interval_ms,
            )
            .max(100),
            ingest_disconnect_grace_ms: env_u64(
                "RESTREAM_INGEST_DISCONNECT_GRACE_MS",
                defaults.ingest_disconnect_grace_ms,
            ),
            output_max_retries: env_u32("RESTREAM_OUTPUT_MAX_RETRIES", defaults.output_max_retries),
            output_retry_base_ms: env_u64(
                "RESTREAM_OUTPUT_RETRY_BASE_MS",
                defaults.output_retry_base_ms,
            )
            .max(1),
            output_retry_max_ms: env_u64(
                "RESTREAM_OUTPUT_RETRY_MAX_MS",
                defaults.output_retry_max_ms,
            )
            .max(1),
            hls_idle_timeout_ms: env_u64(
                "RESTREAM_HLS_IDLE_TIMEOUT_MS",
                defaults.hls_idle_timeout_ms,
            )
            .max(1),
        }
    }
}

impl BackendPolicy {
    pub fn from_env() -> Self {
        Self {
            internal_video_presets: env_bool("RESTREAM_INTERNAL_VIDEO_PRESETS").unwrap_or(false),
            internal_hevc_to_h264: env_bool("RESTREAM_INTERNAL_HEVC_TO_H264").unwrap_or(false),
            internal_hls_preview: env_bool("RESTREAM_INTERNAL_HLS_PREVIEW").unwrap_or(false),
            internal_complex_audio: env_bool("RESTREAM_INTERNAL_AUDIO_COMPLEX").unwrap_or(false),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let ports = ServerPorts {
            http: 3030,
            rtmp: 1935,
            srt: 10080,
        };
        let tuning = RuntimeTuning::default();
        let cpus = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        let derived_permits = cpus.saturating_sub(2).max(1).div_ceil(2).max(1);

        Self {
            ports,
            http_bind_addr: "127.0.0.1".to_string(),
            tuning,
            db_path: "data.db".to_string(),
            media_dir: "media".to_string(),
            log_retention_days: 7,
            backend_policy: BackendPolicy {
                internal_video_presets: false,
                internal_hevc_to_h264: false,
                internal_hls_preview: false,
                internal_complex_audio: false,
            },
            rtmp_backlog: 1024,
            ffmpeg_threads: None,
            avio_capacity: 512 * 1024,
            hls_min_segment_ms: 1.0,
            hls_segment_capacity_bytes: 8 * 1024 * 1024,
            hls_max_segments: 20,
            recording_threads: None,
            ts_ring_capacity: 256,
            ring_capacity: 1024,
            transcoder_ring_capacity: 512,
            require_srt_bonding: false,
            external_ffmpeg_permits: derived_permits,
            ffmpeg_bin_path: None,
            log_dir: "logs".to_string(),
            no_color: false,
            srt_passphrase: None,
            srt_pbkeylen: 16,
            use_internal_file_ingest: false,
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        let ports = ServerPorts::from_env();
        let http_bind_addr =
            std::env::var("RESTREAM_HTTP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let tuning = RuntimeTuning::from_env();
        let db_path = std::env::var("RESTREAM_DB_PATH").unwrap_or_else(|_| "data.db".to_string());
        let media_dir = std::env::var("RESTREAM_MEDIA_DIR").unwrap_or_else(|_| "media".to_string());
        let log_retention_days = env_u64("RESTREAM_LOG_RETENTION_DAYS", 7);
        let backend_policy = BackendPolicy::from_env();
        let rtmp_backlog = env_u32("RESTREAM_RTMP_LISTENER_BACKLOG", 1024);
        let ffmpeg_threads = std::env::var("RESTREAM_EXTERNAL_FFMPEG_THREADS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let avio_capacity = env_usize("RESTREAM_AVIO_QUEUE_CAPACITY", 512 * 1024)
            .clamp(64 * 1024, 16 * 1024 * 1024);

        let hls_min_segment_ms = env_u64("RESTREAM_HLS_MIN_SEGMENT_MS", 1000) as f64 / 1000.0;
        let hls_segment_capacity_bytes =
            env_usize("RESTREAM_HLS_SEGMENT_CAPACITY_BYTES", 8 * 1024 * 1024).max(188);
        let hls_max_segments = env_usize("RESTREAM_HLS_MAX_SEGMENTS", 20).max(1);

        let recording_threads = std::env::var("RESTREAM_RECORDING_FFMPEG_THREADS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let ts_ring_capacity = env_usize("RESTREAM_TS_RING_CAPACITY", 256).clamp(32, 16384);
        let ring_capacity = env_usize("RESTREAM_RING_CAPACITY", 1024).clamp(64, 16384);
        let transcoder_ring_capacity =
            env_usize("RESTREAM_TRANSCODER_RING_CAPACITY", 512).clamp(64, 16384);
        let require_srt_bonding = std::env::var_os("RESTREAM_REQUIRE_SRT_BONDING").is_some();
        let ffmpeg_bin_path = std::env::var("FFMPEG_BIN_PATH").ok();
        let log_dir = std::env::var("RESTREAM_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let srt_passphrase = std::env::var("RESTREAM_SRT_PASSPHRASE").ok();
        let srt_pbkeylen = std::env::var("RESTREAM_SRT_PBKEYLEN")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(16);
        let use_internal_file_ingest =
            std::env::var_os("RESTREAM_USE_INTERNAL_FILE_INGEST").is_some();

        // Calculate external_ffmpeg_permits:
        let permits = if let Ok(value) = std::env::var("RESTREAM_EXTERNAL_FFMPEG_PERMITS")
            && let Some(v) = value.parse::<usize>().ok().filter(|&v| v >= 1)
        {
            v
        } else {
            let cpus = std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1);
            let reserve = std::env::var("RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(2)
                .min(cpus.saturating_sub(1));
            let per_child = std::env::var("RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(2)
                .max(1);
            let hard_cap = std::env::var("RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            let derived = cpus
                .saturating_sub(reserve)
                .max(1)
                .div_ceil(per_child)
                .max(1);
            derived.min(hard_cap).max(1)
        };

        Self {
            ports,
            http_bind_addr,
            tuning,
            db_path,
            media_dir,
            log_retention_days,
            backend_policy,
            rtmp_backlog,
            ffmpeg_threads,
            avio_capacity,
            hls_min_segment_ms,
            hls_segment_capacity_bytes,
            hls_max_segments,
            recording_threads,
            ts_ring_capacity,
            ring_capacity,
            transcoder_ring_capacity,
            require_srt_bonding,
            external_ffmpeg_permits: permits,
            ffmpeg_bin_path,
            log_dir,
            no_color,
            srt_passphrase,
            srt_pbkeylen,
            use_internal_file_ingest,
        }
    }

    pub fn effective_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "ports": {
                "http": self.ports.http,
                "rtmp": self.ports.rtmp,
                "srt": self.ports.srt,
                "httpBindAddr": self.http_bind_addr,
            },
            "tuning": {
                "nofileLimit": self.tuning.nofile_limit,
                "reconcilerIntervalMs": self.tuning.reconciler_interval_ms,
                "ingestDisconnectGraceMs": self.tuning.ingest_disconnect_grace_ms,
                "outputMaxRetries": self.tuning.output_max_retries,
                "outputRetryBaseMs": self.tuning.output_retry_base_ms,
                "outputRetryMaxMs": self.tuning.output_retry_max_ms,
                "hlsIdleTimeoutMs": self.tuning.hls_idle_timeout_ms,
            },
            "paths": {
                "db": self.db_path,
                "media": self.media_dir,
                "logs": self.log_dir,
                "ffmpegBin": self.ffmpeg_bin_path,
            },
            "logging": {
                "retentionDays": self.log_retention_days,
                "noColor": self.no_color,
            },
            "backendPolicy": {
                "internalVideoPresets": self.backend_policy.internal_video_presets,
                "internalHevcToH264": self.backend_policy.internal_hevc_to_h264,
                "internalHlsPreview": self.backend_policy.internal_hls_preview,
                "internalComplexAudio": self.backend_policy.internal_complex_audio,
                "useInternalFileIngest": self.use_internal_file_ingest,
            },
            "ffmpeg": {
                "externalPermits": self.external_ffmpeg_permits,
                "threads": self.ffmpeg_threads,
                "recordingThreads": self.recording_threads,
            },
            "buffers": {
                "avioCapacity": self.avio_capacity,
                "hlsMinSegmentMs": self.hls_min_segment_ms,
                "hlsSegmentCapacityBytes": self.hls_segment_capacity_bytes,
                "hlsMaxSegments": self.hls_max_segments,
                "tsRingCapacity": self.ts_ring_capacity,
                "ringCapacity": self.ring_capacity,
                "transcoderRingCapacity": self.transcoder_ring_capacity,
            },
            "srt": {
                "requireBonding": self.require_srt_bonding,
                "passphraseConfigured": self.srt_passphrase.is_some(),
                "pbkeylen": self.srt_pbkeylen,
            },
            "rtmp": {
                "backlog": self.rtmp_backlog,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_vars(vars: &[(&str, &str)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = vars
            .iter()
            .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        unsafe {
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
        }
        f();
        unsafe {
            for (name, value) in previous {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn with_env_overlay(vars: &[(&str, &str)], removed: &[&str], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous_vars = vars
            .iter()
            .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        let previous_removed = removed
            .iter()
            .map(|name| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        unsafe {
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
            for name in removed {
                std::env::remove_var(name);
            }
        }
        f();
        unsafe {
            for (name, value) in previous_vars.into_iter().chain(previous_removed) {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    #[test]
    fn server_ports_are_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_HTTP_PORT", "4040"),
                ("RESTREAM_RTMP_PORT", "2935"),
                ("RESTREAM_SRT_PORT", "11080"),
            ],
            || {
                let ports = ServerPorts::from_env();
                assert_eq!(ports.http, 4040);
                assert_eq!(ports.rtmp, 2935);
                assert_eq!(ports.srt, 11080);
            },
        );
    }

    #[test]
    fn http_bind_addr_defaults_to_loopback_and_can_be_overridden() {
        with_env_overlay(&[], &["RESTREAM_HTTP_BIND_ADDR"], || {
            assert_eq!(AppConfig::from_env().http_bind_addr, "127.0.0.1");
        });
        with_env_vars(&[("RESTREAM_HTTP_BIND_ADDR", "0.0.0.0")], || {
            assert_eq!(AppConfig::from_env().http_bind_addr, "0.0.0.0");
        });
    }

    #[test]
    fn runtime_tuning_is_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_NOFILE_LIMIT", "1234"),
                ("RESTREAM_RECONCILE_INTERVAL_MS", "5"),
                ("RESTREAM_OUTPUT_RETRY_BASE_MS", "0"),
                ("RESTREAM_HLS_IDLE_TIMEOUT_MS", "90000"),
            ],
            || {
                let tuning = RuntimeTuning::from_env();
                assert_eq!(tuning.nofile_limit, 1234);
                assert_eq!(tuning.reconciler_interval_ms, 100);
                assert_eq!(tuning.output_retry_base_ms, 1);
                assert_eq!(tuning.hls_idle_timeout_ms, 90000);
            },
        );
    }

    #[test]
    fn backend_policy_is_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_INTERNAL_VIDEO_PRESETS", "true"),
                ("RESTREAM_INTERNAL_HEVC_TO_H264", "1"),
                ("RESTREAM_INTERNAL_HLS_PREVIEW", "yes"),
                ("RESTREAM_INTERNAL_AUDIO_COMPLEX", "off"),
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert!(policy.internal_video_presets);
                assert!(policy.internal_hevc_to_h264);
                assert!(policy.internal_hls_preview);
                assert!(!policy.internal_complex_audio);
            },
        );
    }

    #[test]
    fn legacy_global_internal_transcoder_env_does_not_enable_stage_families() {
        with_env_overlay(
            &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
            &[
                "RESTREAM_INTERNAL_VIDEO_PRESETS",
                "RESTREAM_INTERNAL_HEVC_TO_H264",
                "RESTREAM_INTERNAL_HLS_PREVIEW",
                "RESTREAM_INTERNAL_AUDIO_COMPLEX",
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert_eq!(policy, BackendPolicy::default());
            },
        );
    }

    #[test]
    fn backend_policy_does_not_use_global_internal_switch_for_all_stages() {
        with_env_overlay(
            &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
            &[
                "RESTREAM_INTERNAL_VIDEO_PRESETS",
                "RESTREAM_INTERNAL_HEVC_TO_H264",
                "RESTREAM_INTERNAL_HLS_PREVIEW",
                "RESTREAM_INTERNAL_AUDIO_COMPLEX",
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert!(!policy.internal_video_presets);
                assert!(!policy.internal_hevc_to_h264);
                assert!(!policy.internal_hls_preview);
                assert!(!policy.internal_complex_audio);
            },
        );
    }

    #[test]
    fn effective_summary_covers_runtime_knobs_without_secret_values() {
        let config = AppConfig {
            srt_passphrase: Some("super-secret".to_string()),
            ffmpeg_bin_path: Some("/usr/bin/ffmpeg".to_string()),
            ..AppConfig::default()
        };

        let summary = config.effective_summary();
        assert_eq!(summary["ports"]["http"], 3030);
        assert_eq!(summary["tuning"]["reconcilerIntervalMs"], 1000);
        assert_eq!(summary["paths"]["ffmpegBin"], "/usr/bin/ffmpeg");
        assert_eq!(summary["backendPolicy"]["internalHlsPreview"], false);
        assert_eq!(
            summary["ffmpeg"]["externalPermits"],
            config.external_ffmpeg_permits
        );
        assert_eq!(summary["buffers"]["ringCapacity"], 1024);
        assert_eq!(summary["srt"]["passphraseConfigured"], true);
        assert_eq!(summary["srt"]["pbkeylen"], 16);
        assert!(!summary.to_string().contains("super-secret"));
    }
}
