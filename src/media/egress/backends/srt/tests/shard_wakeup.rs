use super::super::*;
use super::support::{FakeReadinessPoller, common, feed};
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::SrtMessageSender;
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct DeadlineWakeSender {
    deadline: Option<Instant>,
    wakeups: Arc<AtomicUsize>,
}

impl SrtMessageSender for DeadlineWakeSender {
    fn send_message(&mut self, _message: &Bytes) -> crate::media::srt::SrtSendResult {
        crate::media::srt::SrtSendResult::WouldBlock
    }

    fn close(&mut self, _reason: crate::media::egress::backend::CloseReason) {}

    fn next_timer_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn on_wakeup(&mut self) {
        self.wakeups.fetch_add(1, Ordering::Relaxed);
        self.deadline = None;
    }
}

#[test]
fn idle_transport_deadline_is_serviced_without_fd_readiness() {
    let poller = FakeReadinessPoller::default();
    let wakeups = Arc::new(AtomicUsize::new(0));
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    backend
        .add_leaf(
            42,
            SrtFabricLeaf::new(
                common(7),
                Box::new(DeadlineWakeSender {
                    deadline: Some(Instant::now() - Duration::from_millis(1)),
                    wakeups: Arc::clone(&wakeups),
                }),
            ),
        )
        .unwrap();

    assert!(backend.next_wakeup().is_some());
    assert_eq!(backend.on_wakeup(), EgressShardCommandEffect::Continue);
    assert_eq!(wakeups.load(Ordering::Relaxed), 1);
    assert!(backend.next_wakeup().is_none());
}
