//! Per-client RTMP connection slots, shared by every ingress owner.
//!
//! The listener-wide connection cap is a resource every client shares; this
//! bounds what one client can take of it. A client is an IPv4 address, or an
//! IPv6 /64 (one host is routinely given a whole /64, so a per-address IPv6
//! cap would not bound anything). A slot is held for the connection's whole
//! life and released by `Drop`, on every exit path including an unwind.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ClientKey {
    V4(u32),
    V6Prefix64(u64),
}

impl ClientKey {
    fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(u32::from(v4)),
                None => Self::V6Prefix64(u64::try_from(u128::from(v6) >> 64).unwrap_or(u64::MAX)),
            },
        }
    }
}

#[derive(Debug)]
pub(super) struct ClientSlots {
    per_client: usize,
    held: Mutex<HashMap<ClientKey, usize>>,
}

impl ClientSlots {
    pub(super) fn new(per_client: usize) -> Arc<Self> {
        Arc::new(Self {
            per_client: per_client.max(1),
            held: Mutex::new(HashMap::new()),
        })
    }

    /// A slot for one connection from `ip`, or `None` when that client
    /// already holds its share.
    pub(super) fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<ClientSlot> {
        let key = ClientKey::of(ip);
        let mut held = crate::sync::lock(&self.held);
        let count = held.entry(key).or_insert(0);
        if *count >= self.per_client {
            return None;
        }
        *count += 1;
        Some(ClientSlot {
            slots: Arc::clone(self),
            key,
        })
    }

    #[cfg(test)]
    fn held_by(&self, ip: IpAddr) -> usize {
        let held = crate::sync::lock(&self.held);
        held.get(&ClientKey::of(ip)).copied().unwrap_or(0)
    }
}

/// One held connection slot; released on drop.
#[derive(Debug)]
pub(super) struct ClientSlot {
    slots: Arc<ClientSlots>,
    key: ClientKey,
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        let mut held = crate::sync::lock(&self.slots.held);
        if let Some(count) = held.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                held.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn one_client_is_capped_and_others_are_not_affected() {
        let slots = ClientSlots::new(2);
        let first = slots.try_acquire(ip("192.0.2.1")).unwrap();
        let _second = slots.try_acquire(ip("192.0.2.1")).unwrap();
        assert!(slots.try_acquire(ip("192.0.2.1")).is_none());
        assert!(slots.try_acquire(ip("192.0.2.2")).is_some());

        drop(first);
        assert!(slots.try_acquire(ip("192.0.2.1")).is_some());
    }

    #[test]
    fn an_ipv6_slash_64_is_one_client_and_mapped_ipv4_is_its_address() {
        let slots = ClientSlots::new(1);
        let _held = slots.try_acquire(ip("2001:db8:1:2::1")).unwrap();
        assert!(slots.try_acquire(ip("2001:db8:1:2:ffff::9")).is_none());
        assert!(slots.try_acquire(ip("2001:db8:1:3::1")).is_some());

        let _v4 = slots.try_acquire(ip("198.51.100.7")).unwrap();
        assert!(slots.try_acquire(ip("::ffff:198.51.100.7")).is_none());
    }

    #[test]
    fn a_slot_is_released_when_its_holder_unwinds() {
        let slots = ClientSlots::new(1);
        let client = ip("203.0.113.5");
        let held = slots.try_acquire(client).unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _slot = held;
            std::panic::resume_unwind(Box::new("connection unwound"));
        }));
        assert!(unwound.is_err());
        assert_eq!(slots.held_by(client), 0);
    }
}
