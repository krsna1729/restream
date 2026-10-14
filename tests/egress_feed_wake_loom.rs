//! Loom model-checks for the egress feed wake-coalescing protocol.
//!
//! This file owns the cross-thread contract behind
//! `media::egress::journal::WakeGate` plus the shard wake delivery mechanism:
//!
//! - publisher: advance the feed head, then `notify()` — an `AcqRel` swap
//!   whose clear-to-set transition obligates one bounded delivery attempt;
//! - queued wake: clear pending before reading the head and draining;
//! - Full: clear pending and arm a separate retry bit with an `AcqRel` swap;
//! - fallback: swap-clear retry before reading heads, leaving a queued wake's
//!   pending gate untouched. The bounded idle deadline services a quiet feed.
//!
//! Lost-wakeup safety requires delivery pairing **and** clear-before-observe
//! ordering. The queued-wake models encode sleep as a Condvar wait, where
//! loom's deadlock detector catches stranded data. The Full-retry model uses
//! bounded service turns, including one after publishing ends, to model the
//! idle deadline without relying on another publication.
//!
//! The model mirrors `WakeGate`'s orderings rather than importing it (std
//! atomics vs loom atomics), matching the repository's loom convention — keep
//! it in sync with `src/media/egress/journal.rs`.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

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

    /// Several publishers may share a cloneable shard wake handle. Model
    /// two independent feed heads to ensure coalescing cannot strand either
    /// one. The production loop clears the gate before the backend reads
    /// heads, as the deterministic two-head shard regression also checks.
    #[test]
    fn loom_two_heads_sharing_one_gate_strand_no_publication() {
        loom::model(|| {
            let gate = FeedWakeModel::new();
            let heads = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
            let publishers: Vec<_> = (0..2)
                .map(|feed| {
                    let (gate, heads) = (gate.clone(), heads.clone());
                    thread::spawn(move || {
                        heads[feed].store(1, Ordering::Release);
                        if !gate.pending.swap(true, Ordering::AcqRel) {
                            let mut delivered = gate.delivered.lock().unwrap();
                            *delivered += 1;
                            gate.wake.notify_all();
                        }
                    })
                })
                .collect();

            let mut cursors = [0usize; 2];
            let mut wakes_seen = 0usize;
            loop {
                let _had_wake = gate.pending.swap(false, Ordering::AcqRel);
                for (feed, cursor) in cursors.iter_mut().enumerate() {
                    let head = heads[feed].load(Ordering::Acquire);
                    if head > *cursor {
                        *cursor = head;
                    }
                }
                if cursors == [1, 1] {
                    break;
                }
                let mut delivered = gate.delivered.lock().unwrap();
                while *delivered == wakes_seen {
                    delivered = gate.wake.wait(delivered).unwrap();
                }
                wakes_seen = *delivered;
            }
            for publisher in publishers {
                publisher.join().unwrap();
            }
        });
    }

    /// A full command queue preserves one retry bit for the active shard's
    /// next turn. Clear that bit before reading the head so a publish racing
    /// the read remains pending. Two bounded turns plus the final turn after
    /// the publisher finishes model eventual service, including the idle
    /// deadline when no further command or publication arrives.
    #[test]
    fn loom_full_queue_retry_cannot_lose_the_final_head() {
        loom::model(|| {
            let head = Arc::new(AtomicUsize::new(0));
            let retry = Arc::new(AtomicBool::new(false));
            let (published, full_retry) = (head.clone(), retry.clone());
            let publisher = thread::spawn(move || {
                for sequence in 1..=2 {
                    published.store(sequence, Ordering::Release);
                    full_retry.swap(true, Ordering::AcqRel);
                    thread::yield_now();
                }
            });
            let mut consumed = 0;
            for _ in 0..2 {
                if retry.swap(false, Ordering::AcqRel) {
                    consumed = consumed.max(head.load(Ordering::Acquire));
                }
                thread::yield_now();
            }
            publisher.join().unwrap();
            if retry.swap(false, Ordering::AcqRel) {
                consumed = consumed.max(head.load(Ordering::Acquire));
            }
            assert_eq!(consumed, 2, "the final full-queue hint must survive");
        });
    }
}
