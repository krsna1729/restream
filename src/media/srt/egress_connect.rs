//! Restream's SRT egress URL/session semantics as typed `srt-transport`
//! configuration.
//!
//! A resolved SRT output becomes a real [`CallerConfig`] (one peer) or
//! [`BondedCallerConfig`] (`bond=` legs) with `SocketOwnership::Shared`, ready
//! for `Owner::connect` / `Owner::connect_bonded`. Protocol options are owned
//! by the upstream typed configuration; nothing here re-implements them.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use srt_proto::handshake::GroupType;
use srt_transport::{
    BondedCallerConfig, CallerConfig, EncryptionConfig, GroupConfig, SessionConfig,
    SocketBufferConfig, SocketOwnership,
};

/// The IP family of one shared caller socket. A shard owns at most one Owner
/// (one caller UDP socket) per family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AddressFamily {
    V4,
    V6,
}

impl AddressFamily {
    pub(crate) const COUNT: usize = 2;

    pub(crate) fn of(addr: SocketAddr) -> Self {
        if addr.is_ipv6() { Self::V6 } else { Self::V4 }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::V4 => 0,
            Self::V6 => 1,
        }
    }
}

/// One output's connect request, bound to the family of its (single) socket.
pub(crate) enum SrtConnectKind {
    // Boxed: a direct caller's configuration is far larger than a bond's leg
    // list, and a connect request is control-plane, not per-packet.
    Direct(Box<CallerConfig>),
    Bonded(BondedCallerConfig),
}

pub(crate) struct SrtConnectRequest {
    pub(crate) family: AddressFamily,
    pub(crate) kind: SrtConnectKind,
}

#[derive(Clone)]
pub(crate) struct SrtFabricEgressConnectSpec {
    peer_hosts: Vec<String>,
    stream_id: String,
    passphrase: Option<String>,
    key_length: Option<srt_proto::crypto::KeyLength>,
    bond_type: GroupType,
    /// This output's own connect attempt duration
    /// (`LeafPolicy.connect_timeout`). It becomes the request-local
    /// `CallerConfig.connect.attempt_deadline` of every leg; the Owner's pool
    /// starts its clock at ADMISSION, so queue wait never consumes it.
    connect_timeout: Duration,
}

impl SrtFabricEgressConnectSpec {
    pub(crate) fn from_url(url: &str, connect_timeout: Duration) -> Self {
        let clean = url.strip_prefix("srt://").unwrap_or(url);
        let mut parts = clean.splitn(2, '?');
        let host = parts.next().unwrap_or_default().to_string();
        let mut stream_id = String::new();
        let mut passphrase = None;
        let mut key_length = None;
        let mut bond_type = GroupType::Backup;
        let mut peers = vec![host];
        if let Some(query) = parts.next() {
            for pair in query.split('&') {
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                match key {
                    "streamid" => stream_id = percent_decode(value),
                    "passphrase" => passphrase = Some(percent_decode(value)),
                    "pbkeylen" => {
                        key_length = value
                            .parse::<usize>()
                            .ok()
                            .and_then(srt_proto::crypto::KeyLength::from_len)
                    }
                    "bond" => peers.extend(value.split(',').map(str::to_string)),
                    "type" => match value.to_ascii_lowercase().as_str() {
                        "broadcast" => bond_type = GroupType::Broadcast,
                        "backup" => bond_type = GroupType::Backup,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        Self {
            peer_hosts: peers,
            stream_id,
            passphrase,
            key_length,
            bond_type,
            connect_timeout: connect_timeout.max(Duration::from_millis(1)),
        }
    }

    pub(crate) fn peer_hosts(&self) -> &[String] {
        &self.peer_hosts
    }

    fn session(&self) -> SessionConfig {
        let mut session = SessionConfig::default();
        session.set_stream_id((!self.stream_id.is_empty()).then(|| self.stream_id.clone()));
        if let Some(passphrase) = self.passphrase.as_deref() {
            let mut encryption = EncryptionConfig::new(passphrase);
            if let Some(key_length) = self.key_length {
                encryption = encryption.key_length(key_length);
            }
            session.set_encryption(Some(encryption));
        }
        // The protocol's own whole-handshake deadline must not end the attempt
        // before this output's connect window does.
        session.handshake.timeout = self.connect_timeout.max(session.handshake.retry_interval);
        session
    }

    fn leg(&self, peer: SocketAddr) -> Result<CallerConfig, String> {
        let buffer = std::num::NonZeroUsize::new(super::desired_udp_buf());
        CallerConfig::builder(peer)
            .ownership(SocketOwnership::Shared)
            .session(self.session())
            .connect_deadline(self.connect_timeout)
            .configure_transport(|transport| {
                if let Some(bytes) = buffer {
                    transport.socket_buffers = SocketBufferConfig::Bytes(bytes);
                }
                super::apply_optional_udp_buf(transport);
            })
            .build()
            .map_err(|error| error.to_string())
    }

    /// Translate resolved peers into the typed request for the family Owner.
    ///
    /// A bonded output must live in ONE address family (all its legs share one
    /// Owner's one caller socket); a mixed-family bond is refused explicitly,
    /// never silently truncated or split.
    pub(crate) fn connect_request(
        &self,
        peers: &[SocketAddr],
    ) -> Result<SrtConnectRequest, String> {
        let Some(first) = peers.first().copied() else {
            return Err("SRT connect requires a peer address".to_string());
        };
        let family = AddressFamily::of(first);
        if let Some(other) = peers
            .iter()
            .find(|peer| AddressFamily::of(**peer) != family)
        {
            return Err(format!(
                "bonded SRT output mixes address families ({first} and {other}); all legs of a \
                 bond must be IPv4 or all IPv6"
            ));
        }
        if let [peer] = peers {
            return Ok(SrtConnectRequest {
                family,
                kind: SrtConnectKind::Direct(Box::new(self.leg(*peer)?)),
            });
        }
        if srt_proto::GroupMode::from_group_type(self.bond_type).is_none() {
            return Err("invalid SRT group type".to_string());
        }
        let mut bonded = BondedCallerConfig::new(GroupConfig::new(next_group_id(), self.bond_type));
        for (index, peer) in peers.iter().enumerate() {
            // The first listed leg is preferred: highest weight.
            let weight = u16::try_from(peers.len() - index).unwrap_or(u16::MAX);
            bonded = bonded.leg(self.leg(*peer)?, weight);
        }
        Ok(SrtConnectRequest {
            family,
            kind: SrtConnectKind::Bonded(bonded),
        })
    }

    #[cfg(test)]
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    #[cfg(test)]
    pub(crate) fn bond_type(&self) -> GroupType {
        self.bond_type
    }
}

static NEXT_GROUP_ID: AtomicU32 = AtomicU32::new(10);

fn next_group_id() -> u32 {
    // `GroupConfig::new` applies the wire marker bit.
    NEXT_GROUP_ID.fetch_add(1, Ordering::Relaxed) & !srt_proto::handshake::SRTGROUP_MASK
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(7);

    fn addr(text: &str) -> SocketAddr {
        text.parse().expect("socket address")
    }

    #[test]
    fn parses_stream_id_encryption_and_bond_options() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://a.example:9000?streamid=live%2Fkey&passphrase=secret%20pass&pbkeylen=32&bond=b.example:9001&type=broadcast",
            TIMEOUT,
        );
        assert_eq!(spec.peer_hosts(), ["a.example:9000", "b.example:9001"]);
        assert_eq!(spec.stream_id(), "live/key");
        assert_eq!(spec.bond_type(), GroupType::Broadcast);
    }

    #[test]
    fn single_peer_is_a_direct_shared_caller_in_its_own_family() {
        let spec = SrtFabricEgressConnectSpec::from_url("srt://127.0.0.1:9000?streamid=k", TIMEOUT);
        let request = spec
            .connect_request(&[addr("127.0.0.1:9000")])
            .expect("request");
        assert_eq!(request.family, AddressFamily::V4);
        let SrtConnectKind::Direct(config) = request.kind else {
            panic!("expected a direct caller");
        };
        assert_eq!(config.transport.ownership, SocketOwnership::Shared);
        let v6 = spec.connect_request(&[addr("[::1]:9000")]).expect("v6");
        assert_eq!(v6.family, AddressFamily::V6);
    }

    #[test]
    fn bond_is_one_group_with_first_leg_preferred() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://127.0.0.1:9000?bond=127.0.0.1:9001&type=backup",
            TIMEOUT,
        );
        let request = spec
            .connect_request(&[addr("127.0.0.1:9000"), addr("127.0.0.1:9001")])
            .expect("request");
        let SrtConnectKind::Bonded(config) = request.kind else {
            panic!("expected a bonded caller");
        };
        assert_eq!(config.group.group_type, GroupType::Backup);
        let weights: Vec<u16> = config.legs.iter().map(|leg| leg.weight).collect();
        assert_eq!(weights, vec![2, 1]);
    }

    #[test]
    fn mixed_family_bond_is_refused_explicitly() {
        let spec =
            SrtFabricEgressConnectSpec::from_url("srt://127.0.0.1:9000?bond=[::1]:9001", TIMEOUT);
        let error = spec
            .connect_request(&[addr("127.0.0.1:9000"), addr("[::1]:9001")])
            .err()
            .expect("mixed families are refused");
        assert!(error.contains("mixes address families"), "{error}");
        assert!(spec.connect_request(&[]).is_err());
    }

    fn direct_deadline(spec: &SrtFabricEgressConnectSpec, peer: &str) -> Duration {
        let SrtConnectKind::Direct(config) =
            spec.connect_request(&[addr(peer)]).expect("request").kind
        else {
            panic!("expected a direct caller");
        };
        config.connect.attempt_deadline
    }

    /// A. Each output's own connect timeout is the request's attempt deadline;
    /// two specs on one shard never share one.
    #[test]
    fn a_direct_request_carries_its_own_connect_timeout() {
        let fast = SrtFabricEgressConnectSpec::from_url(
            "srt://127.0.0.1:9000",
            Duration::from_millis(100),
        );
        let slow =
            SrtFabricEgressConnectSpec::from_url("srt://127.0.0.1:9001", Duration::from_secs(5));
        assert_eq!(
            direct_deadline(&fast, "127.0.0.1:9000"),
            Duration::from_millis(100)
        );
        assert_eq!(
            direct_deadline(&slow, "127.0.0.1:9001"),
            Duration::from_secs(5)
        );
        // Owner capacity is not encoded in the request.
        let SrtConnectKind::Direct(config) = fast
            .connect_request(&[addr("127.0.0.1:9000")])
            .expect("request")
            .kind
        else {
            panic!("direct");
        };
        assert_eq!(config.connect.max_in_flight, std::num::NonZeroUsize::MIN);
        assert!(config.session.handshake.timeout >= Duration::from_millis(100));
    }

    /// B. Every leg of a bonded output carries the same request deadline and
    /// upstream preparation accepts the group.
    #[test]
    fn every_bonded_leg_carries_one_identical_deadline() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://127.0.0.1:9000?bond=127.0.0.1:9001,127.0.0.1:9002&type=broadcast",
            Duration::from_millis(2_500),
        );
        let SrtConnectKind::Bonded(config) = spec
            .connect_request(&[
                addr("127.0.0.1:9000"),
                addr("127.0.0.1:9001"),
                addr("127.0.0.1:9002"),
            ])
            .expect("request")
            .kind
        else {
            panic!("bonded");
        };
        assert_eq!(config.legs.len(), 3);
        assert!(
            config
                .legs
                .iter()
                .all(|leg| leg.caller.connect.attempt_deadline == Duration::from_millis(2_500))
        );
        config
            .prepare(srt_transport::RuntimeFlavor::Compio)
            .expect("identical leg deadlines prepare");
    }

    /// H. Different endpoints are never rejected as "different receivers":
    /// only the handshake's peer-group identity decides that.
    #[test]
    fn different_same_family_endpoints_are_a_valid_bond_request() {
        let spec = SrtFabricEgressConnectSpec::from_url(
            "srt://10.0.0.1:9000?bond=10.9.9.9:19000",
            TIMEOUT,
        );
        let request = spec
            .connect_request(&[addr("10.0.0.1:9000"), addr("10.9.9.9:19000")])
            .expect("distinct hosts and ports are accepted");
        assert!(matches!(request.kind, SrtConnectKind::Bonded(_)));
    }
}
