//! Peer-instance port mapping and per-output sink URLs for the resource
//! sweep: which sink instance an output lands on and the URL it publishes to.

use std::path::{Path, PathBuf};

use crate::core::PEER_PORT_STRIDE;

use super::{ResourceSweepEnv, SweepConfig, SweepOutputKind};

/// Ports for peer instance `index` (0-based): each of `mtx_rtmp`/`mtx_rtmps`/
/// `mtx_srt`/`mtx_api` offset by `index * PEER_PORT_STRIDE` (see that constant
/// for the collision invariant). Instance 0 always matches the pre-existing
/// single-mediamtx ports, so `peer_count == 1` is byte-identical to prior
/// behavior.
pub(super) fn peer_instance_ports(env: &ResourceSweepEnv, index: usize) -> (u16, u16, u16, u16) {
    let offset = (index as u16).saturating_mul(PEER_PORT_STRIDE);
    (
        env.mtx_rtmp.wrapping_add(offset),
        env.mtx_rtmps.wrapping_add(offset),
        env.mtx_srt.wrapping_add(offset),
        env.mtx_api.wrapping_add(offset),
    )
}

/// Every port every peer instance binds, checked for overflow and for two roles
/// landing on one port. `ResourceSweepEnv` runs this when it is built, so an
/// explicit `MTX_*` override whose bands overlap fails at startup instead of
/// binding the wrong listener; the collision test calls it too, so the test
/// exercises the same code production runs.
///
/// `mtx_hls` is deliberately absent: it has no per-instance ports, so it cannot
/// collide with a strided band.
pub(super) fn validate_peer_instance_ports(env: &ResourceSweepEnv) -> Result<(), String> {
    let mut seen: Vec<(u16, usize, &'static str)> = Vec::new();
    for index in 0..env.peer_count {
        let offset = u16::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(PEER_PORT_STRIDE))
            .ok_or_else(|| format!("peer instance {index} overflows the port band"))?;
        for (role, base) in [
            ("mtx_rtmp", env.mtx_rtmp),
            ("mtx_rtmps", env.mtx_rtmps),
            ("mtx_srt", env.mtx_srt),
            ("mtx_api", env.mtx_api),
        ] {
            let port = base
                .checked_add(offset)
                .ok_or_else(|| format!("peer instance {index} {role} overflows a port number"))?;
            if let Some((_, previous_index, previous_role)) =
                seen.iter().find(|(candidate, _, _)| *candidate == port)
            {
                return Err(format!(
                    "peer instance {index} {role} and instance {previous_index} {previous_role} \
                     both use port {port}"
                ));
            }
            seen.push((port, index, role));
        }
    }
    Ok(())
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

pub(super) fn resource_output_url(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    // Spread outputs across the `PEER_COUNT` peer instances (stable per
    // output name). A single sink port concentrates every output of a shard
    // onto one SO_REUSEPORT socket: Restream's SRT callers share a UDP socket
    // per shard, so the kernel's 4-tuple hash sees a handful of flows, and at
    // SRT x150 one sink socket dropped 1.08M datagrams while its siblings
    // dropped none. Each instance's ports are owned by one sink thread.
    let (rtmp_port, rtmps_port, srt_port, _) =
        peer_instance_ports(env, peer_instance_for(name, env.peer_count));
    let url = kind.publish_url(rtmp_port, rtmps_port, srt_port, name);
    // SRT outputs can target sink peers on another host
    // (`RESOURCE_SWEEP_SRT_PEER_HOSTS`), which is what the multi-host rungs of
    // the packet-rate ladder need. RTMP output kinds keep loopback peers.
    let url = match (env.srt_peer_host_for(name), kind) {
        (
            Some(host),
            SweepOutputKind::SrtSource | SweepOutputKind::Srt720p | SweepOutputKind::Srt1080p,
        ) => srt_url_on_host(&url, host),
        _ => url,
    };
    (url, kind.encoding(config.multi_audio).to_string())
}

/// Stable peer instance for an output name (FNV-1a), in `0..peer_count`.
pub(super) fn peer_instance_for(name: &str, peer_count: usize) -> usize {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (hash % peer_count.max(1) as u64) as usize
}
