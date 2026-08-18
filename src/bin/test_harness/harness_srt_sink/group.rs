use std::collections::HashMap;
use std::net::SocketAddr;

use shiguredo_srt::{
    GroupExtensionData, GroupMode, HandshakePacket, HandshakeType, SRTGROUP_MASK, SrtGroup,
    SrtPacket,
};

use super::{RustSinkConnection, RustSinkConnectionKey};

#[path = "group_runtime.rs"]
mod runtime;

pub(super) use runtime::{poll_wait, process, receive};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RustSinkGroupKey {
    peer_group_id: u32,
    stream_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct RustSinkGroupRoute {
    group: RustSinkGroupKey,
    member_id: u32,
}

struct RustSinkGroupLeg {
    peer: SocketAddr,
    timers: HashMap<shiguredo_srt::TimerId, shiguredo_srt::Timestamp>,
}

pub(super) struct RustSinkGroup {
    core: SrtGroup,
    local_extension: GroupExtensionData,
    next_member_id: u32,
    legs: HashMap<u32, RustSinkGroupLeg>,
}

pub(super) type RustSinkGroups = HashMap<RustSinkGroupKey, RustSinkGroup>;
pub(super) type RustSinkGroupRoutes = HashMap<RustSinkConnectionKey, RustSinkGroupRoute>;

struct GroupAdmission {
    local_extension: GroupExtensionData,
}

impl RustSinkGroup {
    fn new(local_extension: GroupExtensionData, mode: GroupMode) -> Result<Self, String> {
        let core = SrtGroup::new(local_extension.group_id, mode)
            .map_err(|error| format!("create Rust sink group: {error}"))?;
        Ok(Self {
            core,
            local_extension,
            next_member_id: 1,
            legs: HashMap::new(),
        })
    }

    fn mode(&self) -> GroupMode {
        self.core.mode()
    }

    fn admit_extension(&self, peer: GroupExtensionData) -> GroupExtensionData {
        GroupExtensionData {
            group_id: self.local_extension.group_id,
            group_type: self.local_extension.group_type,
            flags: self.local_extension.flags,
            weight: peer.weight,
        }
    }

    fn add_connection(
        &mut self,
        connection_key: RustSinkConnectionKey,
        connection: RustSinkConnection,
        peer_group: GroupExtensionData,
    ) -> Result<u32, String> {
        let member_id = self.next_member_id;
        self.next_member_id = self.next_member_id.wrapping_add(1).max(1);
        self.core
            .add_member(member_id, peer_group.weight, connection.conn)
            .map_err(|error| format!("add Rust sink group member: {error}"))?;
        self.legs.insert(
            member_id,
            RustSinkGroupLeg {
                peer: connection_key.peer,
                timers: connection.timers,
            },
        );
        Ok(member_id)
    }
}

fn prepare_admission(
    packet: &[u8],
    groups: &mut RustSinkGroups,
    next_group_id: &mut u32,
) -> Result<Option<GroupAdmission>, String> {
    let Some((peer_group, stream_id)) = group_extension_from_packet(packet) else {
        return Ok(None);
    };
    let mode = GroupMode::from_group_type(peer_group.group_type)
        .ok_or_else(|| "Rust sink received an undefined GROUP type".to_string())?;
    let key = RustSinkGroupKey {
        peer_group_id: peer_group.group_id,
        stream_id: normalize_stream_id(stream_id),
    };
    if let Some(group) = groups.get(&key) {
        if group.mode() != mode {
            return Err("Rust sink GROUP type changed for an existing group".to_string());
        }
    } else {
        let local_id = allocate_group_id(next_group_id);
        let local_extension = GroupExtensionData {
            group_id: local_id,
            group_type: peer_group.group_type,
            flags: peer_group.flags,
            weight: 0,
        };
        groups.insert(key.clone(), RustSinkGroup::new(local_extension, mode)?);
    }
    let local_extension = groups
        .get(&key)
        .expect("GROUP admission inserted or found")
        .admit_extension(peer_group);
    Ok(Some(GroupAdmission { local_extension }))
}

fn group_extension_from_packet(packet: &[u8]) -> Option<(GroupExtensionData, Option<String>)> {
    let SrtPacket::Control(control) = SrtPacket::decode(packet).ok()? else {
        return None;
    };
    let handshake = HandshakePacket::decode(&control).ok()?;
    if handshake.handshake_type != HandshakeType::Conclusion {
        return None;
    }
    Some((
        handshake.get_group_extension()?,
        handshake.get_sid_extension(),
    ))
}

fn normalize_stream_id(stream_id: Option<String>) -> Option<String> {
    stream_id.and_then(|stream_id| {
        let normalized = stream_id.trim_matches('\0').trim().to_string();
        (!normalized.is_empty()).then_some(normalized)
    })
}

fn allocate_group_id(next_group_id: &mut u32) -> u32 {
    let low_bits = (*next_group_id & 0x3FFF_FFFF).max(1);
    *next_group_id = next_group_id.wrapping_add(1);
    SRTGROUP_MASK | low_bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_admission_allocates_one_mirror_id_per_peer_group_and_stream() {
        let peer_group = GroupExtensionData {
            group_id: SRTGROUP_MASK | 0x123,
            group_type: shiguredo_srt::GroupType::Broadcast,
            flags: 0,
            weight: 7,
        };
        let mut handshake = HandshakePacket::new_conclusion_request(1, 2, 3, 0, false);
        handshake.add_sid_extension("publish:camera");
        handshake.add_group_extension(peer_group);
        let mut packet = Vec::new();
        handshake.encode(0, 0).encode(&mut packet);

        let mut groups = HashMap::new();
        let mut next_group_id = 9;
        let first = prepare_admission(&packet, &mut groups, &mut next_group_id)
            .expect("valid group admission")
            .expect("group metadata");
        let second = prepare_admission(&packet, &mut groups, &mut next_group_id)
            .expect("valid group admission")
            .expect("group metadata");

        assert_eq!(groups.len(), 1);
        assert_eq!(
            first.local_extension.group_id,
            second.local_extension.group_id
        );
        assert_ne!(first.local_extension.group_id, peer_group.group_id);
        assert_eq!(first.local_extension.weight, peer_group.weight);
    }
}
