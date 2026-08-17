use std::net::SocketAddr;

use crate::media::srt::SrtEgressSendMode;
use crate::media::srt::buffer_sizing::EgressBufferOpts;
use crate::media::srt::srt_crypto::SrtCryptoConfig;
use crate::media::srt::sys::SRTSOCKET;

use super::{
    SrtBondedEgressConnectConfig, SrtEgressMuxerPortClaim, SrtSingleEgressConnectConfig,
    connect_bonded_srt_egress_socket, connect_single_srt_egress_socket,
};

pub(crate) struct SrtFabricEgressConnectConfig<'a> {
    peer_addrs: &'a [SocketAddr],
    stream_id: &'a str,
    crypto: Option<&'a SrtCryptoConfig>,
    connect_timeout_ms: u64,
    muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
    buffer_opts: EgressBufferOpts,
}

impl<'a> SrtFabricEgressConnectConfig<'a> {
    pub(in crate::media::srt) fn new(
        peer_addrs: &'a [SocketAddr],
        stream_id: &'a str,
        crypto: Option<&'a SrtCryptoConfig>,
        connect_timeout_ms: u64,
        muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
        buffer_opts: EgressBufferOpts,
    ) -> Self {
        Self {
            peer_addrs,
            stream_id,
            crypto,
            connect_timeout_ms,
            muxer_port_claim,
            buffer_opts,
        }
    }

    pub(crate) fn peer_addrs(&self) -> &[SocketAddr] {
        self.peer_addrs
    }

    pub(crate) fn stream_id(&self) -> &str {
        self.stream_id
    }

    #[cfg(test)]
    pub(crate) fn connect_timeout_ms(&self) -> u64 {
        self.connect_timeout_ms
    }

    pub(crate) fn crypto_parameters(&self) -> Option<(&str, i32)> {
        self.crypto.as_ref().map(|crypto| crypto.rust_parameters())
    }

    pub(crate) fn buffer_parameters(&self) -> (i32, i32, i32, i64, i32) {
        (
            self.buffer_opts.sndbuf_bytes,
            self.buffer_opts.rcvbuf_bytes,
            self.buffer_opts.latency_ms,
            self.buffer_opts.maxbw_bps,
            self.buffer_opts.fc_pkts,
        )
    }

    #[cfg(test)]
    pub(crate) fn has_muxer_port_claim(&self) -> bool {
        self.muxer_port_claim.is_some()
    }

    #[cfg(test)]
    pub(crate) fn muxer_port_claim_bind_port(&self) -> Option<u16> {
        self.muxer_port_claim
            .as_ref()
            .and_then(SrtEgressMuxerPortClaim::bind_port)
    }
}

pub(crate) fn connect_fabric_srt_egress_socket(
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
            buffer_opts: config.buffer_opts,
        }),
        peer_addrs => ops.connect_bonded(SrtBondedEgressConnectConfig {
            peer_addrs,
            stream_id: config.stream_id,
            crypto: config.crypto,
            send_mode: SrtEgressSendMode::FabricNonblocking,
            buffer_opts: config.buffer_opts,
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
