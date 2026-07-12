//! Canonical SRT URLs used by harness scenarios.

pub(crate) const HARNESS_SRT_PACKET_SIZE: u32 = 1316;
pub(crate) const HARNESS_SRT_LATENCY_US: u32 = 200_000;
pub(crate) const HARNESS_SRT_TIMEOUT_US: u32 = 30_000_000;

#[derive(Clone, Copy)]
pub(crate) enum HarnessSrtMode {
    Publish,
    Read,
}

impl HarnessSrtMode {
    fn streamid(self, stream_key: &str) -> String {
        match self {
            Self::Publish => format!("publish:live/{stream_key}"),
            Self::Read => format!("read:live/{stream_key}"),
        }
    }
}

pub(crate) fn harness_srt_output_url(port: u16, stream_key: &str, mode: HarnessSrtMode) -> String {
    let mut url = format!(
        "srt://127.0.0.1:{port}?streamid={}",
        mode.streamid(stream_key)
    );
    if matches!(mode, HarnessSrtMode::Read) {
        url.push_str(&format!("&timeout={HARNESS_SRT_TIMEOUT_US}"));
    }
    url
}

pub(crate) fn harness_srt_ffmpeg_url(
    port: u16,
    stream_key: &str,
    mode: HarnessSrtMode,
    crypto: Option<(&str, u32)>,
) -> String {
    let mut url = format!(
        "{}&pkt_size={HARNESS_SRT_PACKET_SIZE}&latency={HARNESS_SRT_LATENCY_US}",
        harness_srt_output_url(port, stream_key, mode)
    );
    if matches!(mode, HarnessSrtMode::Read) {
        url.push_str("&mode=caller&transtype=live");
    }
    if let Some((passphrase, pbkeylen)) = crypto {
        url.push_str(&format!("&passphrase={passphrase}&pbkeylen={pbkeylen}"));
    }
    url
}

pub(crate) fn harness_srt_ffmpeg_publish_url(port: u16) -> String {
    format!(
        "srt://127.0.0.1:{port}?pkt_size={HARNESS_SRT_PACKET_SIZE}&latency={HARNESS_SRT_LATENCY_US}"
    )
}

pub(crate) fn harness_srt_ffmpeg_listener_url(port: u16) -> String {
    format!(
        "srt://127.0.0.1:{port}?mode=listener&transtype=live&timeout={HARNESS_SRT_TIMEOUT_US}&latency={HARNESS_SRT_LATENCY_US}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_srt_urls_share_named_defaults() {
        assert_eq!(
            harness_srt_output_url(9000, "out", HarnessSrtMode::Publish),
            "srt://127.0.0.1:9000?streamid=publish:live/out"
        );
        assert_eq!(
            harness_srt_output_url(9000, "out", HarnessSrtMode::Read),
            "srt://127.0.0.1:9000?streamid=read:live/out&timeout=30000000"
        );
        assert_eq!(
            harness_srt_ffmpeg_url(9000, "in", HarnessSrtMode::Publish, None),
            "srt://127.0.0.1:9000?streamid=publish:live/in&pkt_size=1316&latency=200000"
        );
        assert_eq!(
            harness_srt_ffmpeg_listener_url(9000),
            "srt://127.0.0.1:9000?mode=listener&transtype=live&timeout=30000000&latency=200000"
        );
    }
}
