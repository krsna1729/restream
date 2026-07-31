use std::collections::HashSet;
use std::sync::OnceLock;

use crate::SINK_PORT;

/// Concrete restream listener ports for one isolated harness process.
pub(crate) struct TestPorts {
    pub(crate) http: u16,
    pub(crate) rtmp: u16,
    pub(crate) srt: u16,
}

/// Synthesized non-overlapping port ranges for restream, MediaMTX, and probes.
#[derive(Clone, Copy)]
pub(crate) struct HarnessPortDefaults {
    pub(crate) restream_http: u16,
    pub(crate) restream_rtmp: u16,
    pub(crate) restream_srt: u16,
    pub(crate) mtx_rtmp: u16,
    pub(crate) mtx_rtmps: u16,
    pub(crate) mtx_srt: u16,
    pub(crate) mtx_hls: u16,
    pub(crate) mtx_api: u16,
    pub(crate) sink: u16,
    pub(crate) hls_put: u16,
    pub(crate) ffmpeg_srt_sink_base: u16,
    pub(crate) ffmpeg_signal_sink_base: u16,
}

static HARNESS_PORT_DEFAULTS: OnceLock<HarnessPortDefaults> = OnceLock::new();

impl TestPorts {
    pub(crate) fn from_env() -> Self {
        let ports = harness_port_defaults();
        Self {
            http: ports.restream_http,
            rtmp: ports.restream_rtmp,
            srt: ports.restream_srt,
        }
    }
}

pub(crate) fn harness_port_defaults() -> HarnessPortDefaults {
    *HARNESS_PORT_DEFAULTS.get_or_init(|| {
        let mut reserved = HashSet::new();
        HarnessPortDefaults {
            restream_http: env_or_allocated_port("RESTREAM_HTTP", 3030, &mut reserved),
            restream_rtmp: env_or_allocated_port("RESTREAM_RTMP", 1935, &mut reserved),
            restream_srt: env_or_allocated_port("RESTREAM_SRT", 10080, &mut reserved),
            mtx_rtmp: env_or_allocated_port("MTX_RTMP", 1936, &mut reserved),
            mtx_rtmps: env_or_allocated_port("MTX_RTMPS", 1937, &mut reserved),
            mtx_srt: env_or_allocated_port("MTX_SRT", 8891, &mut reserved),
            mtx_hls: env_or_allocated_port("MTX_HLS", 8890, &mut reserved),
            mtx_api: env_or_allocated_port("MTX_API", 9997, &mut reserved),
            sink: env_or_allocated_port_range("SINK_PORT", SINK_PORT, 256, &mut reserved),
            hls_put: env_or_allocated_port_range("HLS_PUT_PORT", 8990, 16, &mut reserved),
            ffmpeg_srt_sink_base: env_or_allocated_port_range(
                "FFMPEG_SRT_SINK_BASE",
                15_000,
                1024,
                &mut reserved,
            ),
            ffmpeg_signal_sink_base: env_or_allocated_port_range(
                "FFMPEG_SIGNAL_SINK_BASE",
                16_000,
                1024,
                &mut reserved,
            ),
        }
    })
}

pub(crate) fn env_or_allocated_port(name: &str, default: u16, reserved: &mut HashSet<u16>) -> u16 {
    env_or_allocated_port_range(name, default, 1, reserved)
}

pub(crate) fn env_or_allocated_port_range(
    name: &str,
    default: u16,
    width: u16,
    reserved: &mut HashSet<u16>,
) -> u16 {
    let width = width.max(1);
    if let Some(port) = std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
    {
        reserve_port_range(port, width, reserved);
        return port;
    }

    let port = synthesized_harness_port_range(name, width, reserved).unwrap_or(default);
    reserve_port_range(port, width, reserved);
    port
}

fn reserve_port_range(start: u16, width: u16, reserved: &mut HashSet<u16>) {
    let width = width.max(1) as u32;
    let start = start as u32;
    for offset in 0..width {
        let candidate = start + offset;
        if candidate > u16::MAX as u32 {
            break;
        }
        reserved.insert(candidate as u16);
    }
}

fn synthesized_harness_port_range(name: &str, width: u16, reserved: &HashSet<u16>) -> Option<u16> {
    // Do not probe-bind here: some restricted runners deny ad hoc socket
    // creation before the harness re-execs into its private loopback namespace.
    // A per-process high-port bundle is enough to avoid host collisions by
    // default while still allowing explicit env overrides when needed.
    let width = width.max(1) as u32;
    let min_port = 20_000u32;
    let max_port = 50_000u32;
    let span = max_port
        .checked_sub(min_port)?
        .checked_sub(width)?
        .checked_add(1)?;
    let pid = std::process::id();
    let name_hash = name.bytes().fold(0u32, |acc, byte| {
        acc.wrapping_mul(33).wrapping_add(byte as u32)
    });
    let base = min_port + pid.wrapping_mul(97).wrapping_add(name_hash) % span;
    for step in 0..1024u32 {
        let candidate = min_port + (base - min_port + step * 37) % span;
        let candidate = candidate as u16;
        let candidate_end = candidate as u32 + width;
        if candidate_end > max_port {
            continue;
        }
        if (0..width).all(|offset| !reserved.contains(&((candidate as u32 + offset) as u16))) {
            return Some(candidate);
        }
    }
    None
}
