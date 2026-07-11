use super::*;

#[cfg(test)]
pub(super) fn video_codec_id(codec: &str) -> Option<ffmpeg_next::ffi::AVCodecID> {
    match codec {
        "h264" | "avc" => Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264),
        "h265" | "hevc" => Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn audio_codec_id(codec: &str) -> Option<ffmpeg_next::ffi::AVCodecID> {
    match codec {
        "aac" => Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_AAC),
        _ => None,
    }
}

/// Read the kernel UDP recv queue occupancy and drop count for a given local port
/// from /proc/net/udp. Returns (rx_queue_bytes, drops).
pub(super) fn read_udp_socket_stats(port: u16) -> Option<(u64, u64)> {
    let port_hex = format!("{:04X}", port);
    let content = std::fs::read_to_string("/proc/net/udp").ok()?;
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 13 {
            continue;
        }
        // local_address is field[1], format "ADDR:PORT" in hex
        if let Some(lport) = fields[1].split(':').nth(1)
            && lport == port_hex
        {
            // rx_queue is second half of field[4] "tx_queue:rx_queue"
            let queues: Vec<&str> = fields[4].split(':').collect();
            let rx_queue = queues
                .get(1)
                .and_then(|s| u64::from_str_radix(s, 16).ok())
                .unwrap_or(0);
            let drops = fields
                .get(12)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            return Some((rx_queue, drops));
        }
    }
    None
}

pub(super) async fn monitor_listener_socket(
    port: u16,
    stats: Arc<crate::media::engine::ListenerSocketStats>,
    effective_udp_recv_capacity: u64,
) {
    use std::sync::atomic::Ordering;

    // The requested 8 MiB may be clamped by the host's rmem_max. Monitor the
    // actual getsockopt value so a constrained host degrades visibly instead
    // of accepting traffic until it drops packets at an understated percentage.
    let configured_buf = effective_udp_recv_capacity.max(1);
    let warn_threshold = configured_buf / 2; // 50%
    let crit_threshold = (configured_buf * 3) / 4; // 75%
    let mut prev_drops = 0u64;
    let mut warned = false;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let (rx_queue, drops) = match read_udp_socket_stats(port) {
            Some(v) => v,
            None => continue,
        };

        stats.rx_queue_bytes.store(rx_queue, Ordering::Relaxed);
        stats.drops.store(drops, Ordering::Relaxed);

        let prev_peak = stats.rx_queue_max_bytes.load(Ordering::Relaxed);
        if rx_queue > prev_peak {
            stats.rx_queue_max_bytes.store(rx_queue, Ordering::Relaxed);
        }

        if drops > prev_drops {
            error!(
                "[srt] ALERT: kernel dropped {} UDP packets on listener :{}  \
                 (total drops: {}, rx_queue: {}KB / {}KB). \
                 Increase net.core.rmem_max and restart, or reduce ingest count.",
                drops - prev_drops,
                port,
                drops,
                rx_queue / 1024,
                configured_buf / 1024,
            );
            prev_drops = drops;
            warned = false; // reset warning so it fires again after drops
        }

        if rx_queue > crit_threshold {
            error!(
                "[srt] ALERT: listener :{} UDP recv queue at {}KB / {}KB ({:.0}%) — \
                 imminent packet loss. Consider reducing concurrent ingest streams \
                 or increasing net.core.rmem_max.",
                port,
                rx_queue / 1024,
                configured_buf / 1024,
                rx_queue as f64 / configured_buf as f64 * 100.0,
            );
            warned = true;
        } else if rx_queue > warn_threshold && !warned {
            error!(
                "[srt] WARNING: listener :{} UDP recv queue at {}KB / {}KB ({:.0}%)",
                port,
                rx_queue / 1024,
                configured_buf / 1024,
                rx_queue as f64 / configured_buf as f64 * 100.0,
            );
            warned = true;
        } else if rx_queue < warn_threshold / 2 {
            warned = false;
        }
    }
}
