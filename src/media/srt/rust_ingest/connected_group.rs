use std::collections::HashMap;
use std::net::SocketAddr;

use mio::net::UdpSocket;
use shiguredo_srt::{
    ConnectionState, GroupExtensionData, GroupMemberState, GroupMode, SrtGroup, Timestamp,
};
use tokio::sync::mpsc::Sender;

use super::connection::{self, RustConnection};
use super::types::{ConnectionId, IngestEvent};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct GroupKey {
    pub(super) group_id: u32,
    pub(super) stream_id: Option<String>,
}

pub(super) struct GroupMember {
    pub(super) physical_id: ConnectionId,
    pub(super) socket_index: usize,
    pub(super) peer: SocketAddr,
    pub(super) timers: HashMap<shiguredo_srt::TimerId, Timestamp>,
}

pub(super) struct ConnectedGroup {
    pub(super) core: SrtGroup,
    pub(super) members: HashMap<u32, GroupMember>,
    pub(super) logical_id: ConnectionId,
    group_type: shiguredo_srt::GroupType,
    next_member_id: u32,
}

impl ConnectedGroup {
    pub(super) fn new(
        extension: GroupExtensionData,
        logical_id: ConnectionId,
    ) -> Result<Self, String> {
        let mode = GroupMode::from_group_type(extension.group_type)
            .ok_or_else(|| "undefined SRT GROUP type".to_string())?;
        let core = SrtGroup::new(extension.group_id, mode)
            .map_err(|error| format!("create SRT ingest group: {error}"))?;
        Ok(Self {
            core,
            members: HashMap::new(),
            logical_id,
            group_type: extension.group_type,
            next_member_id: 1,
        })
    }

    pub(super) fn accepts(&self, extension: GroupExtensionData) -> bool {
        self.core.group_id() == extension.group_id && self.group_type == extension.group_type
    }

    pub(super) fn add_member(
        &mut self,
        connection: RustConnection,
        socket_index: usize,
    ) -> Result<u32, String> {
        let RustConnection {
            id,
            peer,
            core,
            timers,
            authorized: _,
            pending,
            pending_bytes: _,
        } = connection;
        if !pending.is_empty() {
            return Err("SRT GROUP member has unadmitted data".to_string());
        }
        let mut member_id = core.peer_socket_id();
        if member_id == 0 || self.members.contains_key(&member_id) {
            member_id = self.next_member_id;
            while self.members.contains_key(&member_id) {
                member_id = member_id.wrapping_add(1).max(1);
            }
        }
        self.next_member_id = member_id.wrapping_add(1).max(1);
        let weight = core.peer_group_extension().map_or(0, |group| group.weight);
        self.core
            .add_member(member_id, weight, core)
            .map_err(|error| format!("add SRT GROUP member: {error}"))?;
        self.members.insert(
            member_id,
            GroupMember {
                physical_id: id,
                socket_index,
                peer,
                timers,
            },
        );
        Ok(member_id)
    }

    pub(super) fn service_member(
        &mut self,
        member_id: u32,
        socket: &UdpSocket,
        packet: Option<&[u8]>,
        events: &Sender<IngestEvent>,
        now: Timestamp,
    ) -> bool {
        let Some(member) = self.core.member_mut(member_id) else {
            return false;
        };
        if let Some(packet) = packet
            && member.connection_mut().feed_recv_buf(packet, now).is_err()
        {
            self.core.mark_member_broken(member_id);
            return false;
        }
        let Some(leg) = self.members.get_mut(&member_id) else {
            return false;
        };
        if connection::drain_core_outputs(
            member.connection_mut(),
            socket,
            leg.peer,
            &mut leg.timers,
            now,
        )
        .is_err()
        {
            self.core.mark_member_broken(member_id);
            return false;
        }
        self.deliver_data(events, now)
    }

    pub(super) fn service_timer(
        &mut self,
        member_id: u32,
        socket: &UdpSocket,
        events: &Sender<IngestEvent>,
        now: Timestamp,
    ) -> bool {
        let Some(leg) = self.members.get_mut(&member_id) else {
            return false;
        };
        let due: Vec<_> = leg
            .timers
            .iter()
            .filter_map(|(id, deadline)| (now >= *deadline).then_some(*id))
            .collect();
        for id in due {
            leg.timers.remove(&id);
            let Some(member) = self.core.member_mut(member_id) else {
                return false;
            };
            if member.connection_mut().handle_timer(id, now).is_err() {
                self.core.mark_member_broken(member_id);
                return false;
            }
        }
        let Some(leg) = self.members.get_mut(&member_id) else {
            return false;
        };
        let Some(member) = self.core.member_mut(member_id) else {
            return false;
        };
        if connection::drain_core_outputs(
            member.connection_mut(),
            socket,
            leg.peer,
            &mut leg.timers,
            now,
        )
        .is_err()
        {
            self.core.mark_member_broken(member_id);
            return false;
        }
        self.deliver_data(events, now)
    }

    pub(super) fn broken_members(&mut self) -> Vec<u32> {
        self.members
            .keys()
            .copied()
            .filter(|member_id| {
                self.core.member(*member_id).is_none_or(|member| {
                    member.state() == GroupMemberState::Broken
                        || member.connection().state() == ConnectionState::Disconnected
                })
            })
            .collect()
    }

    pub(super) fn remove_member(&mut self, member_id: u32) -> Option<GroupMember> {
        self.core.remove_member(member_id);
        self.members.remove(&member_id)
    }

    fn deliver_data(&mut self, events: &Sender<IngestEvent>, now: Timestamp) -> bool {
        while let Some(packet) = self.core.poll_data(now) {
            if events
                .blocking_send(IngestEvent::Data {
                    id: self.logical_id,
                    payload: packet.payload,
                })
                .is_err()
            {
                return false;
            }
        }
        true
    }
}
