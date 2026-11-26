//! Exponential restart backoff shared by supervised loops and listeners.

use std::time::Duration;

const RESTART_DELAY_MIN: Duration = Duration::from_secs(1);
const RESTART_DELAY_MAX: Duration = Duration::from_secs(30);
/// A loop that ran this long before exiting restarts at the minimum delay.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Exponential restart delay: 1 s doubling to 30 s, reset after a healthy run.
#[derive(Debug)]
pub(crate) struct RestartBackoff {
    next_delay: Duration,
}

impl RestartBackoff {
    pub(crate) fn new() -> Self {
        Self {
            next_delay: RESTART_DELAY_MIN,
        }
    }

    /// The delay before restarting a loop that ran for `ran`.
    pub(crate) fn on_exit(&mut self, ran: Duration) -> Duration {
        if ran >= HEALTHY_RUN {
            self.next_delay = RESTART_DELAY_MIN;
        }
        let delay = self.next_delay;
        self.next_delay = (self.next_delay * 2).min(RESTART_DELAY_MAX);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delay_doubles_to_the_cap_and_resets_after_a_healthy_run() {
        let mut backoff = RestartBackoff::new();
        let quick = Duration::from_secs(1);
        let delays: Vec<u64> = (0..7).map(|_| backoff.on_exit(quick).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        assert_eq!(backoff.on_exit(HEALTHY_RUN), RESTART_DELAY_MIN);
        assert_eq!(backoff.on_exit(quick), Duration::from_secs(2));
    }
}
