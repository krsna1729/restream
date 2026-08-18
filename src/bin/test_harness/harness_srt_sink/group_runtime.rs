use std::net::SocketAddr;
use std::time::{Duration, Instant};

use shiguredo_srt::{ConnectionState, GroupExtensionData, GroupMemberState, TimerId, Timestamp};

use super::super::{RustSinkConnectionKey, RustSinkGroupPoolState, RustSinkOutput};
use super::{
    RustSinkGroupKey, RustSinkGroupRoute, RustSinkGroupRoutes, RustSinkGroups, normalize_stream_id,
    prepare_admission,
};

pub(crate) fn receive(
    peer: SocketAddr,
    packet: &[u8],
    state: &mut RustSinkGroupPoolState<'_>,
    output: RustSinkOutput<'_>,
) {
    let packet_key = super::super::rust_sink_connection_key(peer, packet);
    if let Some(route) = state.group_routes.get(&packet_key).cloned() {
        receive_group_member(route, packet, state.groups, output, state.start);
        return;
    }

    let admission = match prepare_admission(packet, state.groups, state.next_group_id) {
        Ok(admission) => admission,
        Err(error) => {
            tracing::debug!(%error, %peer, "Rust sink rejected GROUP admission");
            return;
        }
    };
    let connection_key = state
        .routes
        .get(&packet_key)
        .copied()
        .or_else(|| {
            state
                .connections
                .contains_key(&packet_key)
                .then_some(packet_key)
        })
        .unwrap_or(packet_key);
    if let std::collections::hash_map::Entry::Vacant(entry) =
        state.connections.entry(connection_key)
    {
        let socket_id = *state.next_socket_id;
        *state.next_socket_id = state.next_socket_id.wrapping_add(1);
        let local_extension = admission
            .as_ref()
            .map(|admission| admission.local_extension);
        entry.insert(super::super::new_rust_sink_connection(
            state.crypto,
            socket_id,
            local_extension,
        ));
        state
            .routes
            .insert(RustSinkConnectionKey { peer, socket_id }, connection_key);
    }

    let now = super::super::timestamp(state.start);
    let failed = {
        let Some(connection) = state.connections.get_mut(&connection_key) else {
            return;
        };
        if let Some(admission) = &admission {
            connection
                .conn
                .set_group_extension(admission.local_extension);
        }
        if let Err(error) = connection.conn.feed_recv_buf(packet, now) {
            tracing::debug!(%error, "Rust harness SRT sink ignored malformed packet");
            true
        } else {
            super::super::drain_rust_outputs_mode(
                &mut connection.conn,
                output,
                &mut connection.timers,
                now,
            )
            .is_err()
        }
    };
    if failed {
        state.connections.remove(&connection_key);
        state.routes.retain(|_, mapped| *mapped != connection_key);
        return;
    }

    let Some(connection) = state.connections.get(&connection_key) else {
        return;
    };
    if connection.conn.state() == ConnectionState::Connected
        && let Some(peer_group) = connection.conn.peer_group_extension()
    {
        promote_connection(
            connection_key,
            peer_group,
            connection.conn.peer_stream_id().map(str::to_owned),
            state,
        );
    }
}

fn receive_group_member(
    route: RustSinkGroupRoute,
    packet: &[u8],
    groups: &mut RustSinkGroups,
    output: RustSinkOutput<'_>,
    start: Instant,
) {
    let Some(group) = groups.get_mut(&route.group) else {
        return;
    };
    let now = super::super::timestamp(start);
    let Some(member) = group.core.member_mut(route.member_id) else {
        return;
    };
    let failed = if let Err(error) = member.connection_mut().feed_recv_buf(packet, now) {
        tracing::debug!(%error, "Rust harness SRT bonded member rejected packet");
        true
    } else {
        let Some(leg) = group.legs.get_mut(&route.member_id) else {
            return;
        };
        super::super::drain_rust_outputs_mode(member.connection_mut(), output, &mut leg.timers, now)
            .is_err()
    };
    if failed {
        group.core.mark_member_broken(route.member_id);
    }
}

fn promote_connection(
    connection_key: RustSinkConnectionKey,
    peer_group: GroupExtensionData,
    stream_id: Option<String>,
    state: &mut RustSinkGroupPoolState<'_>,
) {
    let key = RustSinkGroupKey {
        peer_group_id: peer_group.group_id,
        stream_id: normalize_stream_id(stream_id),
    };
    let Some(connection) = state.connections.remove(&connection_key) else {
        return;
    };
    let Some(group) = state.groups.get_mut(&key) else {
        return;
    };
    let member_id = match group.add_connection(connection_key, connection, peer_group) {
        Ok(member_id) => member_id,
        Err(error) => {
            tracing::debug!(%error, "Rust sink could not join GROUP member");
            return;
        }
    };
    let route = RustSinkGroupRoute {
        group: key.clone(),
        member_id,
    };
    let aliases: Vec<RustSinkConnectionKey> = state
        .routes
        .iter()
        .filter_map(|(alias, mapped)| (*mapped == connection_key).then_some(*alias))
        .collect();
    state.routes.retain(|_, mapped| *mapped != connection_key);
    for alias in aliases {
        state.group_routes.insert(alias, route.clone());
    }
    state.group_routes.insert(connection_key, route);
}

pub(crate) fn process(
    groups: &mut RustSinkGroups,
    group_routes: &mut RustSinkGroupRoutes,
    socket: &super::super::MioUdpSocket,
    now: Timestamp,
) {
    let keys: Vec<RustSinkGroupKey> = groups.keys().cloned().collect();
    for key in keys {
        let Some(group) = groups.get_mut(&key) else {
            continue;
        };
        let member_ids: Vec<u32> = group.legs.keys().copied().collect();
        let mut broken = Vec::new();
        for member_id in member_ids {
            let Some(leg) = group.legs.get_mut(&member_id) else {
                continue;
            };
            let due: Vec<TimerId> = leg
                .timers
                .iter()
                .filter(|(_, deadline)| now.as_micros() >= deadline.as_micros())
                .map(|(id, _)| *id)
                .collect();
            for id in due {
                leg.timers.remove(&id);
                if let Some(member) = group.core.member_mut(member_id)
                    && let Err(error) = member.connection_mut().handle_timer(id, now)
                {
                    tracing::debug!(%error, member_id, "Rust sink bonded timer failed");
                    broken.push(member_id);
                }
            }
            let Some(member) = group.core.member_mut(member_id) else {
                broken.push(member_id);
                continue;
            };
            let output = RustSinkOutput::Datagram {
                socket,
                peer: leg.peer,
            };
            if super::super::drain_rust_outputs_mode(
                member.connection_mut(),
                output,
                &mut leg.timers,
                now,
            )
            .is_err()
            {
                broken.push(member_id);
            }
        }

        while group.core.poll_data(now).is_some() {}
        for member in group.core.members() {
            if member.state() == GroupMemberState::Broken
                || member.connection().state() == ConnectionState::Disconnected
            {
                broken.push(member.id());
            }
        }
        broken.sort_unstable();
        broken.dedup();
        for member_id in broken {
            group.core.remove_member(member_id);
            group.legs.remove(&member_id);
            group_routes.retain(|_, route| route.group != key || route.member_id != member_id);
        }
    }
    groups.retain(|_, group| !group.legs.is_empty());
}

pub(crate) fn poll_wait(groups: &RustSinkGroups, now: Timestamp) -> Duration {
    let micros = groups
        .values()
        .flat_map(|group| group.legs.values())
        .flat_map(|leg| leg.timers.values())
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(20_000)
        .clamp(1, 20_000);
    Duration::from_micros(micros)
}
