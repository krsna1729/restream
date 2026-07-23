#[cfg(test)]
use std::net::SocketAddr;

use crate::media::srt::srt_crypto::{SrtCryptoConfig, srt_crypto_from_url};
use crate::media::srt::srt_url::parse_srt_egress_url;

#[cfg(test)]
use super::{SrtEgressMuxerPortClaim, SrtFabricEgressConnectConfig};

#[derive(Clone)]
pub(crate) struct SrtFabricEgressConnectSpec {
    peer_hosts: Vec<String>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pending SRT connect driver consumes the prepared stream ID in the next Phase 4 slice"
        )
    )]
    stream_id: String,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pending SRT connect driver consumes URL crypto in the next Phase 4 slice"
        )
    )]
    crypto: Option<SrtCryptoConfig>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the pending SRT connect driver consumes connect timeout in the next Phase 4 slice"
        )
    )]
    connect_timeout_ms: u64,
}

impl SrtFabricEgressConnectSpec {
    pub(crate) fn from_url(url: &str, connect_timeout_ms: u64) -> Self {
        let parsed = parse_srt_egress_url(url);
        let mut peer_hosts = Vec::with_capacity(parsed.bond_addrs.len() + 1);
        peer_hosts.push(parsed.host_port);
        peer_hosts.extend(parsed.bond_addrs);

        Self {
            peer_hosts,
            stream_id: parsed.streamid,
            crypto: srt_crypto_from_url(parsed.passphrase, parsed.pbkeylen),
            connect_timeout_ms,
        }
    }

    pub(crate) fn peer_hosts(&self) -> &[String] {
        &self.peer_hosts
    }

    #[cfg(test)]
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
        )
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
    fn srt_fabric_connect_spec_builds_borrowed_resolved_config() {
        let spec =
            SrtFabricEgressConnectSpec::from_url("srt://host:9000?streamid=publish%3Akey", 3500);
        let addrs = ["127.0.0.1:9000".parse().unwrap()];
        let config = spec.connect_config(&addrs, None);

        assert_eq!(config.peer_addrs(), addrs);
        assert_eq!(config.stream_id(), "publish:key");
        assert_eq!(config.connect_timeout_ms(), 3500);
    }
}
