use std::net::SocketAddr;

use crate::media::srt::buffer_sizing::EgressBufferOpts;
use crate::media::srt::srt_crypto::{SrtCryptoConfig, srt_crypto_from_url};
use crate::media::srt::srt_url::{SrtBondMode, parse_srt_egress_url};

use super::{SrtEgressMuxerPortClaim, SrtFabricEgressConnectConfig};

#[derive(Clone)]
pub(crate) struct SrtFabricEgressConnectSpec {
    peer_hosts: Vec<String>,
    stream_id: String,
    bond_mode: SrtBondMode,
    crypto: Option<SrtCryptoConfig>,
    connect_timeout_ms: u64,
    /// Resolved SRT socket options for this destination: formula/constant
    /// defaults (`EgressBufferOpts::defaults`, see `buffer_sizing.rs`), with
    /// any explicit `sndbuf=`/`rcvbuf=`/`latency=`/`maxbw=`/`fc=` URL
    /// overrides applied on top.
    buffer_opts: EgressBufferOpts,
}

impl SrtFabricEgressConnectSpec {
    pub(crate) fn from_url(url: &str, connect_timeout_ms: u64) -> Self {
        let parsed = parse_srt_egress_url(url);
        let mut peer_hosts = Vec::with_capacity(parsed.bond_addrs.len() + 1);
        peer_hosts.push(parsed.host_port);
        peer_hosts.extend(parsed.bond_addrs);

        let buffer_opts = EgressBufferOpts::defaults(None).with_overrides(
            parsed.sndbuf_bytes,
            parsed.rcvbuf_bytes,
            parsed.latency_ms,
            parsed.maxbw_bps,
            parsed.fc_pkts,
        );

        Self {
            peer_hosts,
            stream_id: parsed.streamid,
            bond_mode: parsed.bond_mode,
            crypto: srt_crypto_from_url(parsed.passphrase, parsed.pbkeylen),
            connect_timeout_ms,
            buffer_opts,
        }
    }

    pub(crate) fn peer_hosts(&self) -> &[String] {
        &self.peer_hosts
    }

    pub(crate) fn connect_config<'a>(
        &'a self,
        peer_addrs: &'a [SocketAddr],
        muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
    ) -> SrtFabricEgressConnectConfig<'a> {
        SrtFabricEgressConnectConfig::new(
            peer_addrs,
            &self.stream_id,
            self.crypto.as_ref(),
            self.connect_timeout_ms,
            muxer_port_claim,
            self.buffer_opts,
            self.bond_mode,
        )
    }

    #[cfg(test)]
    pub(in crate::media::srt) fn buffer_opts(&self) -> EgressBufferOpts {
        self.buffer_opts
    }

    #[cfg(test)]
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    #[cfg(test)]
    pub(crate) fn has_crypto(&self) -> bool {
        self.crypto.is_some()
    }

    #[cfg(test)]
    pub(crate) fn connect_timeout_ms(&self) -> u64 {
        self.connect_timeout_ms
    }

    #[cfg(test)]
    pub(crate) fn bond_mode(&self) -> SrtBondMode {
        self.bond_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_fabric_connect_spec_preserves_primary_then_bond_hosts() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://primary:9000?streamid=publish%3Akey&bond=backup1:9001,backup2:9002",
            1500,
        );

        assert_eq!(
            spec.peer_hosts(),
            &[
                "primary:9000".to_string(),
                "backup1:9001".to_string(),
                "backup2:9002".to_string(),
            ]
        );
        assert_eq!(spec.stream_id(), "publish:key");
        assert_eq!(spec.connect_timeout_ms(), 1500);
    }

    #[test]
    fn srt_fabric_connect_spec_keeps_url_crypto_private() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://host:9000?streamid=publish:key&passphrase=s3cret%20value&pbkeylen=24",
            2500,
        );

        assert!(spec.has_crypto());
    }

    #[test]
    fn srt_fabric_connect_spec_preserves_broadcast_bond_mode() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://host:9000?streamid=publish:key&bondmode=broadcast",
            2500,
        );

        assert_eq!(spec.bond_mode(), SrtBondMode::Broadcast);
        assert_eq!(
            spec.connect_config(&[], None).bond_mode(),
            SrtBondMode::Broadcast
        );
    }

    #[test]
    fn srt_fabric_connect_spec_builds_borrowed_resolved_config() {
        let spec =
            SrtFabricEgressConnectSpec::from_url("srt://host:9000?streamid=publish%3Akey", 3500);
        let addrs = ["127.0.0.1:9000".parse().unwrap()];
        let config = spec.connect_config(&addrs, None);

        assert_eq!(config.peer_addrs(), addrs);
        assert_eq!(config.stream_id(), "publish:key");
        assert_eq!(config.connect_timeout_ms(), 3500);
    }

    #[test]
    fn sndbuf_url_override_takes_precedence_over_formula_default() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://host:9000?streamid=publish:key&sndbuf=3000000",
            1500,
        );
        assert_eq!(spec.buffer_opts().sndbuf_bytes, 3_000_000);
    }

    #[test]
    fn sndbuf_falls_back_to_formula_default_when_absent_from_url() {
        let spec =
            SrtFabricEgressConnectSpec::from_url("srt://host:9000?streamid=publish:key", 1500);
        assert_eq!(
            spec.buffer_opts().sndbuf_bytes,
            crate::media::srt::buffer_sizing::EgressBufferOpts::defaults(None).sndbuf_bytes
        );
    }

    #[test]
    fn latency_maxbw_rcvbuf_fc_overrides_all_take_precedence_over_defaults() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://host:9000?streamid=publish:key&rcvbuf=500000&latency=400&maxbw=6250000&fc=8192",
            1500,
        );
        let opts = spec.buffer_opts();
        assert_eq!(opts.rcvbuf_bytes, 500_000);
        assert_eq!(opts.latency_ms, 400);
        assert_eq!(opts.maxbw_bps, 6_250_000);
        assert_eq!(opts.fc_pkts, 8192);
    }

    #[test]
    fn link_opts_fall_back_to_defaults_when_absent_from_url() {
        let spec =
            SrtFabricEgressConnectSpec::from_url("srt://host:9000?streamid=publish:key", 1500);
        let defaults = crate::media::srt::buffer_sizing::EgressBufferOpts::defaults(None);
        assert_eq!(spec.buffer_opts(), defaults);
    }
}
