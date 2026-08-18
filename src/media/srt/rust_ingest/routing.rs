pub(super) use srt_lifecycle::{
    GroupAffinity, LogicalGroupKey, RoutingMode, WorkerRouter, handshake_route, worker_count,
};

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

        assert_eq!(router.release(&first), None);
        let third_worker =
            router.assign(third, Some(group.clone()), super::RoutingMode::RoundRobin);
        assert_eq!(third_worker, second_worker);

        assert_eq!(router.release(&second), None);
        assert_eq!(router.release(&third), Some(group.logical_key()));
        assert_eq!(router.active_tuple_count(), 0);
        assert_eq!(router.active_group_count(), 0);
    }

    #[test]
    fn group_worker_affinity_includes_stream_id() {
        let mut router = super::WorkerRouter::new(2);
        let first = SocketAddr::from(([127, 0, 0, 1], 21_001));
        let second = SocketAddr::from(([127, 0, 0, 1], 21_002));
        let base = GroupExtensionData {
            group_id: 0x4000_0042,
            group_type: GroupType::Broadcast,
            flags: 0,
            weight: 7,
        };
        let first_group = super::GroupAffinity {
            group_id: base.group_id,
            stream_id: Some("publish:first".to_string()),
            extension: base,
        };
        let second_group = super::GroupAffinity {
            group_id: base.group_id,
            stream_id: Some("publish:second".to_string()),
            extension: base,
        };

        let first_worker = router.assign(
            first,
            Some(first_group.clone()),
            super::RoutingMode::RoundRobin,
        );
        let second_worker = router.assign(
            second,
            Some(second_group.clone()),
            super::RoutingMode::RoundRobin,
        );
        assert_ne!(first_worker, second_worker);

        let third = SocketAddr::from(([127, 0, 0, 1], 21_003));
        assert_eq!(
            router.assign(third, Some(first_group), super::RoutingMode::LeastTuples),
            first_worker
        );
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
