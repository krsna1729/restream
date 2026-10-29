//! Restream's SRT egress URL/session semantics as typed `srt-transport`
//! configuration.
pub(crate) use restream_srt::address_family::AddressFamily;
pub(crate) use restream_srt::egress_connect::*;

/// Resolve application-owned tuning before constructing transport configuration.
pub(crate) fn egress_connect_spec(
    url: &str,
    connect_timeout: std::time::Duration,
) -> SrtFabricEgressConnectSpec {
    SrtFabricEgressConnectSpec::from_url(url, connect_timeout)
        .with_udp_buffer(super::knobs::desired_udp_buf())
}
