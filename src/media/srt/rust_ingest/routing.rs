use std::collections::HashMap;
use std::net::SocketAddr;

use shiguredo_srt::{GroupExtensionData, HandshakePacket, HandshakeType, SrtPacket};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GroupAffinity {
    pub(super) group_id: u32,
    pub(super) stream_id: Option<String>,
    pub(super) extension: GroupExtensionData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoutingMode {
    RoundRobin,
    LeastTuples,
}

pub(super) fn worker_count(requested: usize, available_parallelism: usize) -> usize {
    requested.max(1).min(available_parallelism.max(1))
}

pub(super) struct WorkerRouter {
    tuple_workers: HashMap<SocketAddr, usize>,
    tuple_groups: HashMap<SocketAddr, u32>,
    group_workers: HashMap<u32, usize>,
    group_tuple_counts: HashMap<u32, usize>,
    worker_tuple_counts: Vec<usize>,
    next_worker: usize,
}

impl WorkerRouter {
    pub(super) fn new(worker_count: usize) -> Self {
        Self {
            tuple_workers: HashMap::new(),
            tuple_groups: HashMap::new(),
            group_workers: HashMap::new(),
            group_tuple_counts: HashMap::new(),
            worker_tuple_counts: vec![0; worker_count.max(1)],
            next_worker: 0,
        }
    }

    pub(super) fn assign(
        &mut self,
        peer: SocketAddr,
        group: Option<GroupAffinity>,
        mode: RoutingMode,
    ) -> usize {
        if let Some(worker) = self.tuple_workers.get(&peer).copied() {
            if let Some(group) = group {
                self.register_group(peer, worker, group);
            }
            return worker;
        }

        let worker = group
            .as_ref()
            .and_then(|key| self.group_workers.get(&key.group_id).copied())
            .unwrap_or_else(|| self.select_worker(mode));
        self.tuple_workers.insert(peer, worker);
        self.worker_tuple_counts[worker] = self.worker_tuple_counts[worker].saturating_add(1);
        if let Some(group) = group {
            self.register_group(peer, worker, group);
        }
        worker
    }

    pub(super) fn release(&mut self, peer: SocketAddr) -> Option<u32> {
        let worker = self.tuple_workers.remove(&peer)?;
        self.worker_tuple_counts[worker] = self.worker_tuple_counts[worker].saturating_sub(1);
        if let Some(group_id) = self.tuple_groups.remove(&peer)
            && let Some(count) = self.group_tuple_counts.get_mut(&group_id)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.group_tuple_counts.remove(&group_id);
                self.group_workers.remove(&group_id);
                return Some(group_id);
            }
        }
        None
    }

    #[cfg(test)]
    pub(super) fn active_tuple_count(&self) -> usize {
        self.tuple_workers.len()
    }

    #[cfg(test)]
    pub(super) fn active_group_count(&self) -> usize {
        self.group_workers.len()
    }

    fn register_group(&mut self, peer: SocketAddr, worker: usize, group: GroupAffinity) {
        if self.tuple_groups.contains_key(&peer) {
            return;
        }
        self.group_workers.entry(group.group_id).or_insert(worker);
        self.group_tuple_counts
            .entry(group.group_id)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        self.tuple_groups.insert(peer, group.group_id);
    }

    fn select_worker(&mut self, mode: RoutingMode) -> usize {
        let worker = match mode {
            RoutingMode::RoundRobin => self.next_worker % self.worker_tuple_counts.len(),
            RoutingMode::LeastTuples => {
                let mut selected = self.next_worker % self.worker_tuple_counts.len();
                for offset in 1..self.worker_tuple_counts.len() {
                    let candidate = (self.next_worker + offset) % self.worker_tuple_counts.len();
                    if self.worker_tuple_counts[candidate] < self.worker_tuple_counts[selected] {
                        selected = candidate;
                    }
                }
                selected
            }
        };
        self.next_worker = worker.wrapping_add(1);
        worker
    }
}

pub(super) fn handshake_route(packet: &[u8]) -> Option<(bool, Option<GroupAffinity>)> {
    let SrtPacket::Control(control) = SrtPacket::decode(packet).ok()? else {
        return None;
    };
    let handshake = HandshakePacket::decode(&control).ok()?;
    let is_conclusion = matches!(handshake.handshake_type, HandshakeType::Conclusion);
    let group = handshake
        .get_group_extension()
        .map(|extension| GroupAffinity {
            group_id: extension.group_id,
            stream_id: handshake.get_sid_extension(),
            extension,
        });
    Some((is_conclusion, group))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use shiguredo_srt::{GroupExtensionData, GroupType, HandshakePacket, SRTGROUP_MASK};

    #[test]
    fn worker_count_never_exceeds_cpu_budget_or_reaches_zero() {
        assert_eq!(super::worker_count(0, 8), 1);
        assert_eq!(super::worker_count(2, 8), 2);
        assert_eq!(super::worker_count(99, 4), 4);
        assert_eq!(super::worker_count(99, 0), 1);
    }

    #[test]
    fn group_affinity_survives_member_disconnect_and_overrides_new_tuple_routing() {
        let mut router = super::WorkerRouter::new(4);
        let first = SocketAddr::from(([127, 0, 0, 1], 20_001));
        let second = SocketAddr::from(([192, 0, 2, 1], 20_002));
        let third = SocketAddr::from(([198, 51, 100, 1], 20_003));
        let group = super::GroupAffinity {
            group_id: 0x4000_0042,
            stream_id: Some("publish:camera".to_string()),
            extension: GroupExtensionData {
                group_id: 0x4000_0042,
                group_type: GroupType::Broadcast,
                flags: 0,
                weight: 7,
            },
        };

        let first_worker =
            router.assign(first, Some(group.clone()), super::RoutingMode::RoundRobin);
        let second_worker =
            router.assign(second, Some(group.clone()), super::RoutingMode::LeastTuples);
        assert_eq!(second_worker, first_worker);

        assert_eq!(router.release(first), None);
        let third_worker =
            router.assign(third, Some(group.clone()), super::RoutingMode::RoundRobin);
        assert_eq!(third_worker, second_worker);

        assert_eq!(router.release(second), None);
        assert_eq!(router.release(third), Some(group.group_id));
        assert_eq!(router.active_tuple_count(), 0);
        assert_eq!(router.active_group_count(), 0);
    }

    #[test]
    fn conclusion_route_exposes_group_and_stream_before_core_admission() {
        let group = GroupExtensionData {
            group_id: SRTGROUP_MASK | 0x42,
            group_type: GroupType::Broadcast,
            flags: 0,
            weight: 7,
        };
        let mut handshake = HandshakePacket::new_conclusion_request(1, 2, 3, 0, false);
        handshake.add_sid_extension("publish:camera");
        handshake.add_group_extension(group);
        let mut packet = Vec::new();
        handshake.encode(0, 0).encode(&mut packet);

        let (is_conclusion, affinity) = super::handshake_route(&packet).expect("handshake route");
        assert!(is_conclusion);
        assert_eq!(affinity.expect("GROUP metadata").group_id, group.group_id);
    }

    #[test]
    fn induction_route_is_available_without_group_metadata() {
        let handshake = HandshakePacket::new_induction_request(1);
        let mut packet = Vec::new();
        handshake.encode(0, 0).encode(&mut packet);

        let (is_conclusion, affinity) = super::handshake_route(&packet).expect("handshake route");
        assert!(!is_conclusion);
        assert!(affinity.is_none());
    }
}
