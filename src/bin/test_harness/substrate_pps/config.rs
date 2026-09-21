//! Configuration and shared primitives for the substrate benchmark.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bytes::Bytes;

use super::super::peer_state::run_id;
use super::super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variant {
    /// The completion-based Compio path as the WI3.5 reference measured it: a
    /// boxed per-datagram future, kept frozen as historical evidence.
    Compio,
    /// WI3.6 control: the same Compio path without per-datagram `Box::pin`
    /// orchestration — one homogeneous future type in `FuturesUnordered`, the
    /// shape the production Owner TX machinery uses. Without this arm, a faster
    /// Stage B could be misread as "negative Owner overhead" when part of the
    /// difference is Stage A's own harness allocation.
    CompioPipeline,
    /// A purpose-built native ring.
    IoUring,
    /// Blocking `libc::sendto` in this process: the control that says whether a
    /// measured rate is a submission-API property or the kernel/UDP stack.
    Sendto,
}

impl Variant {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "compio" => Ok(Self::Compio),
            "compio-pipeline" => Ok(Self::CompioPipeline),
            "io-uring" | "io_uring" => Ok(Self::IoUring),
            "sendto" => Ok(Self::Sendto),
            other => Err(format!(
                "SUBSTRATE_VARIANT must be compio, compio-pipeline, io-uring or sendto, got \
                 {other:?}"
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Compio => "compio",
            Self::CompioPipeline => "compio-pipeline",
            Self::IoUring => "io-uring",
            Self::Sendto => "sendto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReapMode {
    /// One completion per iteration: aligned with the Compio arm, and therefore
    /// one ring enter per datagram.
    Sliding,
    /// Reap the whole window per ring enter: what a batched native ring can do,
    /// at the cost of a different completion pattern.
    Window,
}

impl ReapMode {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sliding" => Ok(Self::Sliding),
            "window" | "batched" => Ok(Self::Window),
            other => Err(format!(
                "SUBSTRATE_REAP_MODE must be sliding or window, got {other:?}"
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Sliding => "sliding",
            Self::Window => "window",
        }
    }
}

pub(crate) struct SubstrateConfig {
    pub(crate) variant: Variant,
    pub(crate) reap_mode: ReapMode,
    /// Exactly one CPU: the whole point is `pps/core`.
    pub(crate) sender_cpus: String,
    pub(crate) harness_cpus: Option<String>,
    pub(crate) payload_bytes: usize,
    pub(crate) destinations: Vec<SocketAddr>,
    pub(crate) queue_depth: usize,
    pub(crate) warmup: Duration,
    pub(crate) duration: Duration,
    pub(crate) report_secs: u64,
    pub(crate) receiver_state: Option<String>,
    pub(crate) run: String,
}

/// `count` IPv4 destinations starting at `base`, all inside the peer's local
/// prefix: destination-address diversity on the TX side without 1000 receiver
/// tasks on the RX side.
pub(crate) fn destinations(base: Ipv4Addr, count: usize, port: u16) -> Vec<SocketAddr> {
    let start = u32::from(base);
    (0..count)
        .map(|index| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(start + index as u32)), port))
        .collect()
}

pub(crate) fn parse_config() -> Result<SubstrateConfig, String> {
    let variant = Variant::parse(&std::env::var("SUBSTRATE_VARIANT").unwrap_or_default())?;
    let sender_cpus = std::env::var("SUBSTRATE_SENDER_CPUS").unwrap_or_else(|_| "0".to_string());
    let sender_cpus = sender_cpus.trim().to_string();
    if sender_cpus.is_empty() || sender_cpus.contains(',') || sender_cpus.contains('-') {
        return Err(format!(
            "SUBSTRATE_SENDER_CPUS must name exactly one CPU (got {sender_cpus:?}): pps/core is \
             only meaningful when the sender owns one core"
        ));
    }
    let harness_cpus = std::env::var("SUBSTRATE_HARNESS_CPUS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(mask) = &harness_cpus
        && mask.split(',').any(|part| part.trim() == sender_cpus)
    {
        return Err(format!(
            "harness CPUs {mask:?} overlap the sender CPU {sender_cpus:?}: the sender must own its \
             core exclusively"
        ));
    }
    let payload_bytes = env_usize("SUBSTRATE_PAYLOAD_BYTES", 1316);
    if payload_bytes == 0 || payload_bytes > 65_507 {
        return Err("SUBSTRATE_PAYLOAD_BYTES must be within 1..=65507".to_string());
    }
    let base: Ipv4Addr = std::env::var("SUBSTRATE_DEST_BASE")
        .unwrap_or_else(|_| "10.53.1.1".to_string())
        .parse()
        .map_err(|e| format!("SUBSTRATE_DEST_BASE: {e}"))?;
    let port = env_usize("SUBSTRATE_DEST_PORT", 9000);
    let port = u16::try_from(port).map_err(|_| "SUBSTRATE_DEST_PORT out of range")?;
    let dest_count = env_usize("SUBSTRATE_DEST_COUNT", 1000).max(1);
    let queue_depth = env_usize("SUBSTRATE_QUEUE_DEPTH", 64).max(1);
    let warmup = Duration::from_secs(env_secs("SUBSTRATE_WARMUP_SECS", 3).max(1));
    let duration = Duration::from_secs(env_secs("SUBSTRATE_DURATION_SECS", 20).max(5));
    let report_secs = env_secs("SUBSTRATE_REPORT_SECS", 5).max(1);
    let receiver_state = std::env::var("SUBSTRATE_RECEIVER_STATE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let reap_mode = ReapMode::parse(
        &std::env::var("SUBSTRATE_REAP_MODE").unwrap_or_else(|_| "sliding".to_string()),
    )?;
    if reap_mode == ReapMode::Window && variant != Variant::IoUring {
        return Err(
            "SUBSTRATE_REAP_MODE=window only applies to the io-uring variant: the Compio arm's \
             completion pattern is part of what it measures"
                .to_string(),
        );
    }
    Ok(SubstrateConfig {
        variant,
        reap_mode,
        sender_cpus,
        harness_cpus,
        payload_bytes,
        destinations: destinations(base, dest_count, port),
        queue_depth,
        warmup,
        duration,
        report_secs,
        receiver_state,
        run: run_id(),
    })
}

/// One preconstructed payload for every datagram: the kernel copies on send, so
/// a single immutable buffer is safe for a whole window of in-flight
/// submissions in both variants.
pub(crate) fn build_payload(bytes: usize) -> Bytes {
    let mut payload = Vec::with_capacity(bytes);
    for index in 0..bytes {
        payload.push((index % 251) as u8);
    }
    Bytes::from(payload)
}

/// Send buffer for both variants, so the comparison is not decided by a default
/// buffer size.
pub(crate) const SEND_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Set a large send buffer on either variant's socket, so the comparison is not
/// decided by a default buffer size.
pub(crate) fn set_send_buffer(fd: libc::c_int, bytes: usize) -> Result<(), String> {
    let value = bytes as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &value as *const _ as *const libc::c_void,
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "setsockopt(SO_SNDBUF): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}
