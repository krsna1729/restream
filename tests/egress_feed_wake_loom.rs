//! Loom model-checks for the egress feed wake-coalescing protocol.
//!
//! This file owns the cross-thread contract behind
//! `media::egress::journal::WakeGate` plus the shard wake delivery mechanism:
//!
//! - publisher: advance the feed head, then `notify()` — an `AcqRel` swap
//!   whose `true` return (clear-to-set transition) obligates delivering
//!   exactly one wake through the shard's wake primitive;
//! - shard: on wake, `take()` (`AcqRel` swap-clear), then re-read the head and
//!   drain; sleep again only on the wake primitive itself.
//!
//! Lost-wakeup safety rests on the delivery pairing, not on flag/head load
//! ordering: every clear-to-set transition delivers one wake, so a shard
//! sleeping with unconsumed data always has a delivery in flight.  The models
//! below encode "sleep" as a real Condvar wait; loom's deadlock detection is
//! the lost-wakeup detector — if any interleaving could strand the shard with
//! unconsumed data and no wake, the model fails to terminate and loom reports
//! it.
//!
//! The model mirrors `WakeGate`'s orderings rather than importing it (std
//! atomics vs loom atomics), matching the repository's loom convention — keep
//! it in sync with `src/media/egress/journal.rs`.

#[cfg(loom)]
mod loom_tests {
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::sync::{Arc, Condvar, Mutex};
    use loom::thread;

    /// Mirror of `(feed head, WakeGate, shard wake primitive)`.
    struct FeedWakeModel {
        /// Ring write index: single publisher, `Release` store / `Acquire` load.
        head: AtomicUsize,
        /// `WakeGate::pending`: `AcqRel` swap on both sides.
        pending: AtomicBool,
        /// Shard wake primitive (models the command channel / poller wake):
        /// count of delivered wakes, plus the condvar the shard sleeps on.
        delivered: Mutex<usize>,
        wake: Condvar,
    }

    impl FeedWakeModel {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                head: AtomicUsize::new(0),
                pending: AtomicBool::new(false),
                delivered: Mutex::new(0),
                wake: Condvar::new(),
            })
        }

        /// Publisher side: advance the head, then notify; deliver one wake on
        /// the clear-to-set transition only (coalescing).
        fn publish(&self, new_head: usize) {
            self.head.store(new_head, Ordering::Release);
            let must_deliver = !self.pending.swap(true, Ordering::AcqRel);
            if must_deliver {
                let mut delivered = self.delivered.lock().unwrap();
                *delivered += 1;
                self.wake.notify_all();
            }
        }

        /// Shard side: drain everything visible after consuming the wake flag.
        /// Returns the new consumed head.
        fn drain(&self, consumed: usize) -> usize {
            let _had_wake = self.pending.swap(false, Ordering::AcqRel);
            let observed = self.head.load(Ordering::Acquire);
            observed.max(consumed)
        }

        /// Shard main loop: consume until `target` units are drained, sleeping
        /// on the wake primitive between visits.  A lost wakeup deadlocks here
        /// and is caught by loom.
        fn run_shard_until(&self, target: usize) {
            let mut consumed = 0usize;
            let mut wakes_seen = 0usize;
            loop {
                consumed = self.drain(consumed);
                if consumed >= target {
                    return;
                }
                let mut delivered = self.delivered.lock().unwrap();
                while *delivered == wakes_seen {
                    delivered = self.wake.wait(delivered).unwrap();
                }
                wakes_seen = *delivered;
            }
        }
    }

    /// Single publish racing the shard's clear-then-observe visit: the shard
    /// must always terminate having consumed the publish.
    #[test]
    fn loom_single_publish_always_reaches_sleeping_shard() {
        loom::model(|| {
            let model = FeedWakeModel::new();
            let publisher_model = model.clone();

            let publisher = thread::spawn(move || {
                publisher_model.publish(1);
            });

            model.run_shard_until(1);
            publisher.join().unwrap();
        });
    }

    /// The interleaving called out by the implementation plan:
    /// 1. publisher advances the head;
    /// 2. shard observes the head;
    /// 3. shard clears `wake_pending`;
    /// 4. publisher advances again.
    ///
    /// Either the second publish delivers a wake or the shard observes the new
    /// head before sleeping; the shard must always consume both units.
    #[test]
    fn loom_second_publish_wakes_or_head_is_observed_before_sleep() {
        loom::model(|| {
            let model = FeedWakeModel::new();
            let publisher_model = model.clone();

            let publisher = thread::spawn(move || {
                publisher_model.publish(1);
                publisher_model.publish(2);
            });

            model.run_shard_until(2);
            publisher.join().unwrap();
        });
    }

    /// Two producer threads sharing one gate (e.g. ingest thread and a timer
    /// republish): coalescing may merge their wakes, but the shard must still
    /// drain both heads.  Exactly one to two wakes are delivered — never zero.
    #[test]
    fn loom_concurrent_publishers_coalesce_without_loss() {
        loom::model(|| {
            let model = FeedWakeModel::new();
            let first = model.clone();
            let second = model.clone();

            // Distinct heads: the shard needs max visibility of both.  Use
            // fetch_add so the two publishers never regress the head.
            let a = thread::spawn(move || {
                first.head.fetch_add(1, Ordering::AcqRel);
                if !first.pending.swap(true, Ordering::AcqRel) {
                    let mut delivered = first.delivered.lock().unwrap();
                    *delivered += 1;
                    first.wake.notify_all();
                }
            });
            let b = thread::spawn(move || {
                second.head.fetch_add(1, Ordering::AcqRel);
                if !second.pending.swap(true, Ordering::AcqRel) {
                    let mut delivered = second.delivered.lock().unwrap();
                    *delivered += 1;
                    second.wake.notify_all();
                }
            });

            model.run_shard_until(2);

            a.join().unwrap();
            b.join().unwrap();

            let delivered = *model.delivered.lock().unwrap();
            assert!(
                (1..=2).contains(&delivered),
                "two publications must deliver one or two wakes, got {delivered}"
            );
        });
    }
}
