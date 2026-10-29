//! Canonical checked-in fixture paths shared by root consumers and leaf tests.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvMarkerBframeMode {
    Bf0,
    Bf2,
}

impl AvMarkerBframeMode {
    const fn file_suffix(self) -> &'static str {
        match self {
            Self::Bf0 => "-bf0",
            Self::Bf2 => "",
        }
    }
}

pub const REQUIRED_CHECKED_IN_FIXTURES: &[&str] = &[
    // Picture-check thumbnails (64x36 grey, made once with the sampler's
    // scaler): drawn slates (colour bars, a card, a gradient) and textured
    // pictures (a fractal, a cell pattern), for the slate test.
    "test/fixtures/stream-qa/slate-bars.gray",
    "test/fixtures/stream-qa/slate-card.gray",
    "test/fixtures/stream-qa/slate-gradient.gray",
    "test/fixtures/stream-qa/textured-fractal.gray",
    "test/fixtures/stream-qa/textured-cells.gray",
    // Two AAC tracks, 6 s: a -20 dBFS tone, then silence. The sound check
    // names the silent second track.
    "test/fixtures/stream-qa/tone-then-silent-track.mp4",
    // Canonical H.264 MPEG-TS correctness source: single-video/single-audio
    // packet fixture used by unit tests and non-measurement protocol gates.
    "test/fixtures/transport/correctness-h264.ts",
    // Canonical HEVC MPEG-TS correctness source: exercises HEVC demux,
    // passthrough, and H.265->H.264 compatibility conversion paths.
    "test/fixtures/transport/correctness-h265.ts",
    // H.264 1.5 Mbps single-audio transport fixture for resource/throughput
    // sweeps where bitrate shape matters more than signal markers.
    "test/fixtures/transport/bench-h264-1_5m.ts",
    // H.264 4 Mbps single-audio transport fixture for medium-bitrate scaling
    // and ramp measurements.
    "test/fixtures/transport/bench-h264-4m.ts",
    // H.264 8 Mbps single-audio transport fixture for high-bitrate scaling
    // and buffer-pressure measurements.
    "test/fixtures/transport/bench-h264-8m.ts",
    // H.264 1.5 Mbps two-audio transport fixture for multi-audio resource
    // sweeps and selected-track routing without synthetic generation.
    "test/fixtures/transport/bench-h264-1_5m-2a.ts",
    // H.264 4 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the medium-bitrate point.
    "test/fixtures/transport/bench-h264-4m-2a.ts",
    // H.264 8 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the high-bitrate point.
    "test/fixtures/transport/bench-h264-8m-2a.ts",
    // HEVC 1.5 Mbps single-audio transport fixture for low-bitrate HEVC
    // scaling and RTMP compatibility-edge measurements.
    "test/fixtures/transport/bench-h265-1_5m.ts",
    // HEVC 4 Mbps single-audio transport fixture for medium-bitrate HEVC
    // scaling and codec-edge resource measurements.
    "test/fixtures/transport/bench-h265-4m.ts",
    // HEVC 8 Mbps single-audio transport fixture for high-bitrate HEVC
    // scaling and stress of the in-process H.265->H.264 path.
    "test/fixtures/transport/bench-h265-8m.ts",
    // HEVC 1.5 Mbps two-audio transport fixture for multi-audio HEVC
    // resource sweeps and selected-track routing.
    "test/fixtures/transport/bench-h265-1_5m-2a.ts",
    // HEVC 4 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the medium-bitrate point.
    "test/fixtures/transport/bench-h265-4m-2a.ts",
    // HEVC 8 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the high-bitrate point.
    "test/fixtures/transport/bench-h265-8m-2a.ts",
    // H.264 marker oracle: black video with periodic white flashes and
    // matching 1 kHz beeps for A/V sync, drift, and PCM-quality assertions.
    "test/fixtures/transport/av-marker-h264.ts",
    // HEVC marker oracle with the same flash/beep timing as the H.264 marker
    // fixture, used to verify signal quality through HEVC paths.
    "test/fixtures/transport/av-marker-h265.ts",
    // H.264 marker oracle with two AAC tracks: track 0 has 1 kHz beeps and
    // track 1 has 2 kHz beeps for multi-audio routing and future frequency
    // identity checks.
    "test/fixtures/transport/av-marker-h264-2a.ts",
    // H.264 marker oracle with B-frames disabled so harnesses can verify
    // source-side no-reorder ingest against the same flash/beep timing.
    "test/fixtures/transport/av-marker-h264-bf0.ts",
    // H.264 two-audio marker oracle with B-frames disabled for multi-audio
    // source-reorder matrix coverage.
    "test/fixtures/transport/av-marker-h264-bf0-2a.ts",
    // HEVC marker oracle with two AAC tracks, covering HEVC multi-audio
    // routing plus RTMP compatibility conversion under signal validation.
    "test/fixtures/transport/av-marker-h265-2a.ts",
    // HEVC marker oracle with B-frames disabled so mixed input can cover
    // HEVC no-reorder ingest without dropping A/V marker signal.
    "test/fixtures/transport/av-marker-h265-bf0.ts",
    // HEVC two-audio marker oracle with B-frames disabled for multi-audio
    // no-reorder ingest coverage.
    "test/fixtures/transport/av-marker-h265-bf0-2a.ts",
    // Sparse-GOP MP4 fixture for file-live-edge optimization tests that verify
    // GOP normalization and recording duration behavior.
    "test/fixtures/media-library/sparse-gop-5s.mp4",
    // Public media-library fixture with two video tracks and sixteen audio
    // tracks; validates high-track-count probing and UI/audio-routing behavior.
    "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
    // MediaMTX sink configuration fixture used by integration harnesses for
    // deterministic local RTMP/SRT/HLS sink behavior.
    "test/harness/mediamtx-sink.yml",
    // Self-signed cert+key for harness RTMPS coverage: mediamtx's RTMP-over-TLS
    // server cert, trusted by restream's egress client via
    // RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM. 20-year validity, CN=localhost.
    "test/fixtures/tls/mediamtx-rtmps-cert.pem",
    "test/fixtures/tls/mediamtx-rtmps-key.pem",
];

fn repo_root() -> PathBuf {
    // This source is also included by leaf-crate tests. Locate the shared
    // source rather than a fixture, so missing fixtures still report paths
    // relative to the repository even when the fixture directory is absent.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|root| root.join("src/test_fixtures/paths.rs").is_file())
        .expect("canonical fixture path source must be inside the repository")
        .to_path_buf()
}

pub fn checked_in_fixture(relative_path: &str) -> Result<PathBuf, String> {
    let path = repo_root().join(relative_path);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "required checked-in fixture missing at {}; restore it from git",
            path.display()
        ))
    }
}

pub fn canonical_h264_ts_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/transport/correctness-h264.ts")
}

pub fn canonical_h265_ts_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/transport/correctness-h265.ts")
}

pub fn sparse_gop_mp4_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/media-library/sparse-gop-5s.mp4")
}

/// Self-signed cert+key pair (`CN=localhost`, SAN `localhost`/`127.0.0.1`,
/// 20-year validity) for harness RTMPS coverage. mediamtx uses it as its
/// RTMP-over-TLS server cert; restream's egress client trusts it via
/// `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` pointed at the same cert file —
/// RTMPS here is server-authenticated TLS only, so no client cert is needed.
pub fn rtmps_harness_cert_fixture() -> Result<(PathBuf, PathBuf), String> {
    Ok((
        checked_in_fixture("test/fixtures/tls/mediamtx-rtmps-cert.pem")?,
        checked_in_fixture("test/fixtures/tls/mediamtx-rtmps-key.pem")?,
    ))
}

pub fn canonical_ts_fixture(codec: &str) -> Result<PathBuf, String> {
    match codec {
        "h264" | "avc" => canonical_h264_ts_fixture(),
        "h265" | "hevc" => canonical_h265_ts_fixture(),
        other => Err(format!("unsupported transport fixture codec {other:?}")),
    }
}

pub fn bench_transport_fixture(
    codec: &str,
    bitrate_label: &str,
    multi_audio: bool,
) -> Result<PathBuf, String> {
    let codec = match codec {
        "h264" | "avc" => "h264",
        "h265" | "hevc" => "h265",
        other => return Err(format!("unsupported benchmark fixture codec {other:?}")),
    };
    let bitrate = bitrate_label.to_ascii_lowercase().replace('.', "_");
    let suffix = if multi_audio { "-2a" } else { "" };
    checked_in_fixture(&format!(
        "test/fixtures/transport/bench-{codec}-{bitrate}{suffix}.ts"
    ))
}

pub fn av_marker_transport_fixture(codec: &str, multi_audio: bool) -> Result<PathBuf, String> {
    av_marker_transport_fixture_for_bframes(codec, multi_audio, AvMarkerBframeMode::Bf2)
}

pub fn av_marker_transport_fixture_for_bframes(
    codec: &str,
    multi_audio: bool,
    bframes: AvMarkerBframeMode,
) -> Result<PathBuf, String> {
    let codec = match codec {
        "h264" | "avc" => "h264",
        "h265" | "hevc" => "h265",
        other => return Err(format!("unsupported A/V marker fixture codec {other:?}")),
    };
    let suffix = if multi_audio { "-2a" } else { "" };
    checked_in_fixture(&format!(
        "test/fixtures/transport/av-marker-{codec}{}{suffix}.ts",
        bframes.file_suffix()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_and_root_manifests_resolve_the_same_checked_in_fixture() {
        let fixture = canonical_h264_ts_fixture().expect("canonical H.264 fixture");
        assert!(fixture.is_absolute());
        let relative = fixture
            .strip_prefix(repo_root())
            .expect("fixture must be relative to the repository")
            .to_str()
            .expect("fixture paths are UTF-8");
        assert!(REQUIRED_CHECKED_IN_FIXTURES.contains(&relative));
    }

    #[test]
    fn missing_fixture_reports_the_repository_path_and_restore_instruction() {
        let relative = "test/fixtures/transport/fixture-resolver-missing.ts";
        let error = checked_in_fixture(relative).expect_err("fixture must not exist");
        assert_eq!(
            error,
            format!(
                "required checked-in fixture missing at {}; restore it from git",
                repo_root().join(relative).display()
            )
        );
    }
}
