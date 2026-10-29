use std::sync::Arc;
use std::time::{Duration, Instant};

use restream_core::MediaPacket;
use restream_sync::Reader;

pub(super) fn load_fixture() -> Vec<u8> {
    let path =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture missing at {}: {e}", path.display()))
}

pub(super) fn load_primary_transport_packets(
    codec: &str,
) -> (
    restream_core::metadata::VideoMeta,
    Vec<restream_core::metadata::AudioMeta>,
    Vec<MediaPacket>,
) {
    restream::test_fixtures::primary_av_packets_for_codec(codec).unwrap_or_else(|e| panic!("{e}"))
}

pub(super) async fn collect_packets_with_deadline(
    reader: &mut Reader,
    min_packets: usize,
    timeout: Duration,
) -> Vec<Arc<MediaPacket>> {
    let deadline = Instant::now() + timeout;
    let mut packets = Vec::new();
    while packets.len() < min_packets && Instant::now() < deadline {
        match reader.pull() {
            Ok(Some(packet)) => packets.push(packet),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    packets
}
