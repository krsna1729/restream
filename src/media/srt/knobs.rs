//! Process-wide A/B knobs for SRT UDP buffers and batch caps.
//!
//! Unset or unparseable values keep the historical defaults. Resolved once
//! per process so the packet path never re-parses the environment.

use std::sync::{LazyLock, OnceLock};

use srt_transport::advanced::driver::RecvBudget;
use tracing::info;

use restream_srt::knobs::{DESIRED_UDP_BUF, resolve_recv_budget};

const UDP_BUF_ENV: &str = "RESTREAM_SRT_UDP_BUF_BYTES";
const RECV_BUDGET_ENV: &str = "RESTREAM_SRT_RECV_BUDGET_DATAGRAMS";

pub(crate) fn desired_udp_buf() -> usize {
    udp_buf_override().unwrap_or(DESIRED_UDP_BUF)
}

pub fn recv_budget() -> RecvBudget {
    recv_budget_or(RecvBudget::default())
}

pub fn recv_budget_or(fallback: RecvBudget) -> RecvBudget {
    static VALUE: OnceLock<Option<RecvBudget>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            resolve_recv_budget(crate::media::config::env_optional_positive_usize(
                RECV_BUDGET_ENV,
            ))
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

fn udp_buf_override() -> Option<usize> {
    static VALUE: LazyLock<Option<usize>> = LazyLock::new(|| {
        crate::media::config::env_optional_positive_usize(UDP_BUF_ENV).inspect(|value| {
            log_override(UDP_BUF_ENV, *value, DESIRED_UDP_BUF);
        })
    });
    *VALUE
}

fn log_override(name: &str, value: usize, default: usize) {
    if value != default {
        info!(target: "restream::media::srt::knobs", env = name, value, default, "SRT A/B knob override");
    }
}
