use std::os::raw::c_int;

use super::srt_stream_id::percent_decode;

/// Explicit per-destination overrides parsed from `srt://` query parameters.
/// Mirrors the URL-query-parameter convention already used by
/// ffmpeg/gstreamer/OBS's SRT handlers and by libsrt's own reference tools
/// for tuning `latency`/`sndbuf`/`rcvbuf`/`maxbw`/`fc` per destination —
/// this repo only recognized `streamid`/`passphrase`/`pbkeylen`/`bond`
/// before. Every field here is `None` (formula/constant default from
/// `EgressBufferOpts::defaults`, see buffer_sizing.rs) unless the operator asked
/// for something different on this one destination.
pub(super) struct SrtEgressUrl {
    pub(super) host_port: String,
    pub(super) streamid: String,
    pub(super) bond_addrs: Vec<String>,
    pub(super) passphrase: String,
    pub(super) pbkeylen: Option<c_int>,
    /// `sndbuf=<bytes>`: SRT send-buffer ceiling (`SRTO_SNDBUF`).
    pub(super) sndbuf_bytes: Option<i32>,
    /// `rcvbuf=<bytes>`: SRT receive-buffer ceiling (`SRTO_RCVBUF`). Rarely
    /// worth raising on egress (a send-dominant socket only receives small
    /// ACK/NAK control traffic) but left overridable for symmetry and for
    /// any future receive-heavy egress shape.
    pub(super) rcvbuf_bytes: Option<i32>,
    /// `latency=<ms>`: timestamp-based-delivery latency window
    /// (`SRTO_LATENCY`). Larger tolerates more jitter/loss at the cost of
    /// end-to-end delay; smaller lowers delay at the cost of ARQ headroom.
    pub(super) latency_ms: Option<i32>,
    /// `maxbw=<bytes-per-sec>`: `SRTO_MAXBW`, in libsrt's own units (bytes/s,
    /// not bits/s — matches the ffmpeg/libsrt URL convention directly, no
    /// conversion). `-1` (the default) means unlimited/input-relative.
    pub(super) maxbw_bps: Option<i64>,
    /// `fc=<packets>`: flow-control window (`SRTO_FC`), i.e. the max number
    /// of in-flight unacknowledged packets libsrt allows.
    pub(super) fc_pkts: Option<i32>,
}

pub(super) fn parse_srt_egress_url(url: &str) -> SrtEgressUrl {
    let url_cleaned = url.replace("srt://", "");
    let parts: Vec<&str> = url_cleaned.split('?').collect();
    let host_port = parts[0].to_string();

    let mut streamid = String::new();
    let mut bond_addrs: Vec<String> = Vec::new();
    let mut passphrase = String::new();
    let mut pbkeylen = None;
    let mut sndbuf_bytes = None;
    let mut rcvbuf_bytes = None;
    let mut latency_ms = None;
    let mut maxbw_bps = None;
    let mut fc_pkts = None;
    if parts.len() > 1 {
        for param in parts[1].split('&') {
            let key_val: Vec<&str> = param.splitn(2, '=').collect();
            if key_val.len() == 2 {
                match key_val[0] {
                    "streamid" => streamid = percent_decode(key_val[1]),
                    "passphrase" => passphrase = percent_decode(key_val[1]),
                    "pbkeylen" => pbkeylen = key_val[1].parse::<c_int>().ok(),
                    "bond" => {
                        bond_addrs = key_val[1].split(',').map(|s| s.to_string()).collect();
                    }
                    "sndbuf" => sndbuf_bytes = key_val[1].parse::<i32>().ok().filter(|v| *v > 0),
                    "rcvbuf" => rcvbuf_bytes = key_val[1].parse::<i32>().ok().filter(|v| *v > 0),
                    "latency" => latency_ms = key_val[1].parse::<i32>().ok().filter(|v| *v >= 0),
                    "maxbw" => maxbw_bps = key_val[1].parse::<i64>().ok().filter(|v| *v >= -1),
                    "fc" => fc_pkts = key_val[1].parse::<i32>().ok().filter(|v| *v > 0),
                    _ => {}
                }
            }
        }
    }
    SrtEgressUrl {
        host_port,
        streamid,
        bond_addrs,
        passphrase,
        pbkeylen,
        sndbuf_bytes,
        rcvbuf_bytes,
        latency_ms,
        maxbw_bps,
        fc_pkts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sndbuf_override_is_parsed() {
        let parsed = parse_srt_egress_url("host:9000?streamid=publish:key&sndbuf=5000000");
        assert_eq!(parsed.sndbuf_bytes, Some(5_000_000));
    }

    #[test]
    fn sndbuf_absent_by_default() {
        let parsed = parse_srt_egress_url("host:9000?streamid=publish:key");
        assert_eq!(parsed.sndbuf_bytes, None);
    }

    #[test]
    fn sndbuf_rejects_non_positive_or_unparseable_values() {
        assert_eq!(
            parse_srt_egress_url("host:9000?sndbuf=0").sndbuf_bytes,
            None
        );
        assert_eq!(
            parse_srt_egress_url("host:9000?sndbuf=-5").sndbuf_bytes,
            None
        );
        assert_eq!(
            parse_srt_egress_url("host:9000?sndbuf=notanumber").sndbuf_bytes,
            None
        );
    }

    #[test]
    fn all_buffer_and_link_overrides_are_parsed_together() {
        let parsed = parse_srt_egress_url(
            "host:9000?streamid=publish:key&sndbuf=3000000&rcvbuf=500000&latency=400&maxbw=6250000&fc=8192",
        );
        assert_eq!(parsed.sndbuf_bytes, Some(3_000_000));
        assert_eq!(parsed.rcvbuf_bytes, Some(500_000));
        assert_eq!(parsed.latency_ms, Some(400));
        assert_eq!(parsed.maxbw_bps, Some(6_250_000));
        assert_eq!(parsed.fc_pkts, Some(8192));
    }

    #[test]
    fn latency_accepts_zero_but_rejects_negative() {
        assert_eq!(
            parse_srt_egress_url("host:9000?latency=0").latency_ms,
            Some(0)
        );
        assert_eq!(
            parse_srt_egress_url("host:9000?latency=-1").latency_ms,
            None
        );
    }

    #[test]
    fn maxbw_accepts_unlimited_sentinel_but_rejects_other_negatives() {
        assert_eq!(
            parse_srt_egress_url("host:9000?maxbw=-1").maxbw_bps,
            Some(-1)
        );
        assert_eq!(parse_srt_egress_url("host:9000?maxbw=-2").maxbw_bps, None);
    }

    #[test]
    fn rcvbuf_and_fc_reject_non_positive_or_unparseable_values() {
        assert_eq!(
            parse_srt_egress_url("host:9000?rcvbuf=0").rcvbuf_bytes,
            None
        );
        assert_eq!(
            parse_srt_egress_url("host:9000?rcvbuf=abc").rcvbuf_bytes,
            None
        );
        assert_eq!(parse_srt_egress_url("host:9000?fc=0").fc_pkts, None);
        assert_eq!(parse_srt_egress_url("host:9000?fc=abc").fc_pkts, None);
    }

    #[test]
    fn unrecognized_params_are_silently_ignored() {
        let parsed = parse_srt_egress_url("host:9000?streamid=publish:key&oheadbw=25&tlpktdrop=0");
        assert_eq!(parsed.streamid, "publish:key");
    }
}
