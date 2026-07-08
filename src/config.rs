//! Centralized application configuration.
//! Reads environment variables once at startup and stores them in a typed struct.

use crate::planner::backend_policy::BackendPolicy;
use crate::{RuntimeTuning, ServerPorts};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ports: ServerPorts,
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
}
