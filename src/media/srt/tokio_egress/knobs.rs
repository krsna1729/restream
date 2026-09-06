//! Process-wide A/B knobs for Tokio SRT UDP buffers and batch caps.
//!
//! Unset or unparseable values keep the historical defaults. Resolved once
//! per process so the packet path never re-parses the environment.

use std::sync::OnceLock;

use srt_transport::{RecvBatch, RecvBudget, SocketBufferConfig, TransportConfig};
use tracing::info;

/// Requested `SO_RCVBUF`/`SO_SNDBUF` for shared Tokio SRT egress sockets.
pub(crate) const DESIRED_UDP_BUF: usize = 8 * 1024 * 1024;
const DEFAULT_IO_BATCH_CAPACITY: usize = 64;
const UDP_BUF_ENV: &str = "RESTREAM_SRT_UDP_BUF_BYTES";
const RECV_BUDGET_ENV: &str = "RESTREAM_SRT_RECV_BUDGET_DATAGRAMS";
const IO_BATCH_ENV: &str = "RESTREAM_SRT_IO_BATCH_CAPACITY";

pub(crate) fn desired_udp_buf() -> usize {
    udp_buf_override().unwrap_or(DESIRED_UDP_BUF)
}

pub(crate) fn shared_io_batch_capacity() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        let value = crate::config::env_optional_positive_usize(IO_BATCH_ENV)
            .unwrap_or(DEFAULT_IO_BATCH_CAPACITY);
        log_override(IO_BATCH_ENV, value, DEFAULT_IO_BATCH_CAPACITY);
        value
    })
}

pub(crate) fn recv_budget() -> RecvBudget {
    recv_budget_or(RecvBudget::default())
}

pub(crate) fn recv_budget_or(fallback: RecvBudget) -> RecvBudget {
    static VALUE: OnceLock<Option<RecvBudget>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            resolve_recv_budget(crate::config::env_optional_positive_usize(RECV_BUDGET_ENV))
                .inspect(|budget| {
                    log_override(
                        RECV_BUDGET_ENV,
                        budget.max_datagrams,
                        fallback.max_datagrams,
                    );
                })
        })
        .unwrap_or(fallback)
}

pub(crate) fn apply_optional_udp_buf(transport: &mut TransportConfig) {
    if let Some(bytes) = udp_buf_override().and_then(std::num::NonZeroUsize::new) {
        transport.socket_buffers = SocketBufferConfig::Bytes(bytes);
    }
}

fn udp_buf_override() -> Option<usize> {
    static VALUE: OnceLock<Option<usize>> = OnceLock::new();
    *VALUE.get_or_init(|| {
        crate::config::env_optional_positive_usize(UDP_BUF_ENV).inspect(|value| {
            log_override(UDP_BUF_ENV, *value, DESIRED_UDP_BUF);
        })
    })
}

fn resolve_recv_budget(datagrams: Option<usize>) -> Option<RecvBudget> {
    let datagrams = datagrams.filter(|&value| value >= 1)?;
    Some(RecvBudget::new(
        datagrams.div_ceil(RecvBatch::DEFAULT_CAPACITY).max(1),
        datagrams,
    ))
}

fn log_override(name: &str, value: usize, default: usize) {
    if value != default {
        info!(env = name, value, default, "SRT A/B knob override");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_recv_budget_matches_srt_rs_default() {
        assert_eq!(resolve_recv_budget(None), None);
        assert_eq!(RecvBudget::default().max_datagrams, 64);
        assert_eq!(RecvBudget::default().max_rounds, 2);
    }

    #[test]
    fn recv_budget_override_scales_rounds_to_cover_datagrams() {
        let small = resolve_recv_budget(Some(8)).expect("parsed");
        assert_eq!(small.max_datagrams, 8);
        assert_eq!(small.max_rounds, 1);
        let large = resolve_recv_budget(Some(256)).expect("parsed");
        assert_eq!(large.max_datagrams, 256);
        assert_eq!(large.max_rounds, 8);
        let default_shaped = resolve_recv_budget(Some(64)).expect("parsed");
        assert_eq!(default_shaped, RecvBudget::default());
    }
}
