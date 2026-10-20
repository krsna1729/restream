//! Wake subscribers shared by a ring's packet publications and its track
//! metadata changes.

use std::sync::Arc;

use arc_swap::ArcSwap;

/// Woken by a ring's producer right after each publication, and when the ring
/// is sealed or ends. Egress fabric shards subscribe with their coalescing
/// wake gates, so a publication reaches them from the producing thread with no
/// watcher task in between (WI11). The same type backs
/// [`super::RingBuffer::metadata_subscribers`], woken only on track metadata
/// changes. Implementations must be cheap and must not block: they run on the
/// producer's hot path.
pub trait PublishWake: Send + Sync {
    fn wake(&self);
}

/// A ring's publication subscribers. A replacement ring shares its
/// predecessor's set (`seal_and_forward`), so wakes follow the feed.
pub struct PublishSubscribers {
    list: ArcSwap<Vec<Arc<dyn PublishWake>>>,
}

impl PublishSubscribers {
    pub(crate) fn new() -> Self {
        Self {
            list: ArcSwap::from_pointee(Vec::new()),
        }
    }

    /// Add `subscriber`; idempotent per `Arc`. Control-plane only (copies
    /// the list).
    pub fn subscribe(&self, subscriber: Arc<dyn PublishWake>) {
        self.list.rcu(|list| {
            let mut list = Vec::clone(list);
            if !list
                .iter()
                .any(|existing| std::ptr::addr_eq(Arc::as_ptr(existing), Arc::as_ptr(&subscriber)))
            {
                list.push(subscriber.clone());
            }
            list
        });
    }

    pub fn unsubscribe(&self, subscriber: &Arc<dyn PublishWake>) {
        self.list.rcu(|list| {
            list.iter()
                .filter(|existing| {
                    !std::ptr::addr_eq(Arc::as_ptr(existing), Arc::as_ptr(subscriber))
                })
                .cloned()
                .collect::<Vec<_>>()
        });
    }

    /// One `ArcSwap` load and one call per subscriber; no allocation.
    #[inline]
    pub(crate) fn wake_all(&self) {
        for subscriber in self.list.load().iter() {
            subscriber.wake();
        }
    }

    pub fn len(&self) -> usize {
        self.list.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
