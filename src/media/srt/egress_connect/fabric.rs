use std::net::SocketAddr;

use crate::media::srt::SrtEgressSendMode;
use crate::media::srt::srt_crypto::SrtCryptoConfig;
use crate::media::srt::sys::SRTSOCKET;

use super::{
    SrtBondedEgressConnectConfig, SrtEgressMuxerPortClaim, SrtSingleEgressConnectConfig,
    connect_bonded_srt_egress_socket, connect_single_srt_egress_socket,
};

pub(in crate::media::srt) struct SrtFabricEgressConnectConfig<'a> {
    pub(in crate::media::srt) peer_addrs: &'a [SocketAddr],
    pub(in crate::media::srt) stream_id: &'a str,
    pub(in crate::media::srt) crypto: Option<&'a SrtCryptoConfig>,
    pub(in crate::media::srt) connect_timeout_ms: u64,
    pub(in crate::media::srt) muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
}

#[allow(dead_code)]
pub(in crate::media::srt) fn connect_fabric_srt_egress_socket(
    config: SrtFabricEgressConnectConfig<'_>,
) -> Result<SRTSOCKET, String> {
    connect_fabric_srt_egress_socket_with(config, LibSrtFabricConnectOps)
}

fn connect_fabric_srt_egress_socket_with<O>(
    config: SrtFabricEgressConnectConfig<'_>,
    mut ops: O,
) -> Result<SRTSOCKET, String>
where
    O: SrtFabricConnectOps,
{
    match config.peer_addrs {
        [] => Err("fabric SRT connect requires at least one peer address".to_string()),
        [peer_addr] => ops.connect_single(SrtSingleEgressConnectConfig {
            peer_addr: *peer_addr,
            stream_id: config.stream_id,
            crypto: config.crypto,
            connect_timeout_ms: config.connect_timeout_ms,
            send_mode: SrtEgressSendMode::FabricNonblocking,
            muxer_port_claim: config.muxer_port_claim,
        }),
        peer_addrs => ops.connect_bonded(SrtBondedEgressConnectConfig {
            peer_addrs,
            stream_id: config.stream_id,
            crypto: config.crypto,
            send_mode: SrtEgressSendMode::FabricNonblocking,
        }),
    }
}

trait SrtFabricConnectOps {
    fn connect_single(
        &mut self,
        config: SrtSingleEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String>;
    fn connect_bonded(
        &mut self,
        config: SrtBondedEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String>;
}

#[allow(dead_code)]
struct LibSrtFabricConnectOps;

impl SrtFabricConnectOps for LibSrtFabricConnectOps {
    fn connect_single(
        &mut self,
        config: SrtSingleEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String> {
        connect_single_srt_egress_socket(config)
    }

    fn connect_bonded(
        &mut self,
        config: SrtBondedEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String> {
        connect_bonded_srt_egress_socket(config)
    }
}

#[cfg(test)]
#[path = "fabric_tests.rs"]
mod tests;
