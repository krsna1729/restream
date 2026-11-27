//! Peer-instance port mapping and per-output sink URLs for the resource
//! sweep: which sink instance an output lands on and the URL it publishes to.

use std::path::{Path, PathBuf};

use crate::core::PEER_PORT_STRIDE;
use crate::harness_srt_sink::sink_slot;

use super::{SweepConfig, SweepOutputKind};

/// The peer instances' port bands: instance 0's MediaMTX ports, the instance
/// count, and how many SRT ports each instance listens on. Instance `index`
/// offsets every port by `index * PEER_PORT_STRIDE` (see that constant for the
/// collision invariant), so instance 0 always matches the single-mediamtx
/// ports and `peer_count == 1` is byte-identical to prior behavior.
#[derive(Clone, Copy, Debug)]
pub(super) struct PeerPorts {
    rtmp: u16,
    rtmps: u16,
    srt: u16,
    api: u16,
    count: usize,
    /// One MediaMTX listener, or `HARNESS_SRT_SINK_PORTS_PER_PEER`
    /// consecutive harness sink ports with one Owner each. Restream sends
    /// every SRT output of an egress shard from one UDP socket, so sink
    /// parallelism must come from ports the harness picks; a kernel
    /// `SO_REUSEPORT` hash sees one flow per shard and once left one Owner a
    /// whole port's load (Q-027).
    srt_per_peer: usize,
}

impl PeerPorts {
    /// `[rtmp, rtmps, srt, api]` are instance 0's ports.
    pub(super) fn new(base: [u16; 4], count: usize, srt_per_peer: usize) -> Self {
        let [rtmp, rtmps, srt, api] = base;
        Self {
            rtmp,
            rtmps,
            srt,
            api,
            count,
            srt_per_peer,
        }
    }

    /// `(rtmp, rtmps, srt, api)` for peer instance `index` (0-based).
    pub(super) fn instance(&self, index: usize) -> (u16, u16, u16, u16) {
        let offset = (index as u16).saturating_mul(PEER_PORT_STRIDE);
        (
            self.rtmp.wrapping_add(offset),
            self.rtmps.wrapping_add(offset),
            self.srt.wrapping_add(offset),
            self.api.wrapping_add(offset),
        )
    }

    /// Every SRT port peer instance `index` listens on.
    pub(super) fn srt_ports(&self, index: usize) -> impl Iterator<Item = u16> {
        let base = self.instance(index).2;
        (0..self.srt_per_peer as u16).map(move |offset| base.wrapping_add(offset))
    }

    /// The SRT port for spread slot `slot` (an output-name hash, or an MSR
    /// ordinal), placed by `sink_slot`, so the instance always matches
    /// `peer_instance_for`/`msr_peer_instance`.
    pub(super) fn srt_port(&self, slot: u64) -> u16 {
        let (instance, offset) = sink_slot(slot, self.count, self.srt_per_peer);
        self.instance(instance).2.wrapping_add(offset)
    }

    /// Every port every peer instance binds, checked for overflow and for two
    /// roles landing on one port. `ResourceSweepEnv` runs this when it is
    /// built, so an explicit `MTX_*` override whose bands overlap fails at
    /// startup instead of binding the wrong listener; the collision test calls
    /// it too, so the test exercises the same code production runs.
    ///
    /// The ports come from `instance` - the mapping the listeners bind - so a
    /// stride change cannot pass this check while the listeners move with it.
    /// `mtx_hls` is deliberately absent (it has no per-instance ports), and
    /// every SRT port (`srt_ports`) is included even though only the TCP roles
    /// can collide on one port: the check is stricter than the kernel requires
    /// and costs nothing.
    pub(super) fn validate(&self) -> Result<(), String> {
        // Overflow precheck for the highest instance, before the mapping's
        // saturating arithmetic could hide an unrepresentable band.
        let last = self.count.saturating_sub(1);
        let max_offset = u16::try_from(last)
            .ok()
            .and_then(|last| last.checked_mul(PEER_PORT_STRIDE))
            .ok_or_else(|| format!("peer instance {last} overflows the port band"))?;
        let srt_span = self.srt_per_peer as u16 - 1;
        for (role, base, span) in [
            ("mtx_rtmp", self.rtmp, 0),
            ("mtx_rtmps", self.rtmps, 0),
            ("mtx_srt", self.srt, srt_span),
            ("mtx_api", self.api, 0),
        ] {
            max_offset
                .checked_add(span)
                .and_then(|top| base.checked_add(top))
                .ok_or_else(|| format!("peer instance {last} {role} overflows a port number"))?;
        }

        let mut seen: Vec<(u16, usize, &'static str)> = Vec::new();
        for index in 0..self.count {
            let (rtmp, rtmps, _, api) = self.instance(index);
            let tcp = [("mtx_rtmp", rtmp), ("mtx_rtmps", rtmps), ("mtx_api", api)];
            let srt = self.srt_ports(index).map(|port| ("mtx_srt", port));
            for (role, port) in tcp.into_iter().chain(srt) {
                if let Some((_, previous_index, previous_role)) =
                    seen.iter().find(|(candidate, _, _)| *candidate == port)
                {
                    return Err(format!(
                        "peer instance {index} {role} and instance {previous_index} \
                         {previous_role} both use port {port}"
                    ));
                }
                seen.push((port, index, role));
            }
        }
        Ok(())
    }

    /// The publish URL and encoding for output `name`. Outputs spread across
    /// the `PEER_COUNT` peer instances and SRT outputs across each instance's
    /// sink ports, stable per output name: a sink port per instance once took
    /// every output of a shard on one socket, and at SRT x150 one dropped
    /// 1.08M datagrams while its siblings dropped none (see `srt_per_peer`).
    /// SRT outputs move to `srt_host` when one is given
    /// (`RESOURCE_SWEEP_SRT_PEER_HOSTS`), which is what the multi-host rungs
    /// of the packet-rate ladder need; RTMP output kinds keep loopback peers.
    pub(super) fn output_url(
        &self,
        srt_host: Option<&str>,
        config: SweepConfig,
        kind: SweepOutputKind,
        name: &str,
    ) -> (String, String) {
        let (rtmp_port, rtmps_port, _, _) = self.instance(peer_instance_for(name, self.count));
        let srt_port = self.srt_port(name_slot(name));
        let url = kind.publish_url(rtmp_port, rtmps_port, srt_port, name);
        let url = match (srt_host, kind) {
            (
                Some(host),
                SweepOutputKind::SrtSource | SweepOutputKind::Srt720p | SweepOutputKind::Srt1080p,
            ) => srt_url_on_host(&url, host),
            _ => url,
        };
        (url, kind.encoding(config.multi_audio).to_string())
    }
}

/// Suffix `path` with `-{index}` (before the extension) for `index > 0`,
/// leaving `index == 0` untouched so instance-0 artifact filenames stay
/// stable for existing tooling and single-instance runs.
pub(super) fn instance_suffixed_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("instance");
    let file_name = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => format!("{stem}-{index}.{ext}"),
        None => format!("{stem}-{index}"),
    };
    path.with_file_name(file_name)
}

/// Move one harness-built SRT publish URL onto a remote peer host. Only the
/// loopback authority the harness itself builds is replaced, and IPv6 hosts are
/// bracketed, so a rung against `RESOURCE_SWEEP_SRT_PEER_HOSTS` reaches a real
/// remote sink without touching any other part of the URL.
pub(super) fn srt_url_on_host(url: &str, host: &str) -> String {
    let authority = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    url.replacen("srt://127.0.0.1:", &format!("srt://{authority}:"), 1)
}

/// Stable peer instance for an output name, in `0..peer_count`.
pub(super) fn peer_instance_for(name: &str, peer_count: usize) -> usize {
    (name_slot(name) % peer_count.max(1) as u64) as usize
}

/// Stable spread slot for an output name (FNV-1a).
fn name_slot(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
