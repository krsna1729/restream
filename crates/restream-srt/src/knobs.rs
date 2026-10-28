//! Pure SRT transport defaults and receive-budget configuration.

use srt_transport::advanced::driver::RecvBudget;
use srt_transport::advanced::native_io::RecvBatch;

/// Requested `SO_RCVBUF`/`SO_SNDBUF` for the SRT Owner sockets.
pub const DESIRED_UDP_BUF: usize = 8 * 1024 * 1024;

/// Scale receive rounds to cover the requested number of datagrams.
/// `None` or zero leaves the caller's transport default untouched.
pub fn resolve_recv_budget(datagrams: Option<usize>) -> Option<RecvBudget> {
    let datagrams = datagrams.filter(|&value| value >= 1)?;
    Some(RecvBudget::new(
        datagrams.div_ceil(RecvBatch::DEFAULT_CAPACITY).max(1),
        datagrams,
    ))
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
