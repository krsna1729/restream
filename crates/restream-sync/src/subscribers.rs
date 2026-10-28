//! Wake subscribers shared by a ring's packet publications and its track
//! metadata changes.

use arc_swap::ArcSwap;
use std::sync::Arc;

pub trait PublishWake: Send + Sync {
    fn wake(&self);
}

pub struct PublishSubscribers {
    list: ArcSwap<Vec<Arc<dyn PublishWake>>>,
}

impl Default for PublishSubscribers {
    fn default() -> Self {
        Self::new()
    }
}

impl PublishSubscribers {
    pub fn new() -> Self {
        Self {
            list: ArcSwap::from_pointee(Vec::new()),
        }
    }

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

    pub fn wake_all(&self) {
        for s in self.list.load().iter() {
            s.wake();
        }
    }
    pub fn len(&self) -> usize {
        self.list.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.load().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyWake;
    impl PublishWake for DummyWake {
        fn wake(&self) {}
    }

    #[test]
    fn subscribers_len_and_empty() {
        let subs = PublishSubscribers::default();
        assert!(subs.is_empty());
        assert_eq!(subs.len(), 0);

        let sub1: Arc<dyn PublishWake> = Arc::new(DummyWake);
        subs.subscribe(sub1.clone());
        assert!(!subs.is_empty());
        assert_eq!(subs.len(), 1);

        subs.unsubscribe(&sub1);
        assert!(subs.is_empty());
        assert_eq!(subs.len(), 0);
    }
}
