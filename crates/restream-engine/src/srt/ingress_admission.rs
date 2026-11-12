//! Synchronous SRT ingress admission policy, resolved on the Owner thread.
//!
//! `srt_transport::compio::Owner::listen_with_resolver` calls this after cookie
//! validation and before CONCLUSION is processed. It reads only the already
//! populated [`SrtIngestPolicyStore`] and a non-blocking IP-ban check; the
//! asynchronous control-plane checks (pipeline authentication, the
//! authoritative IP-ban check, duplicate publishers) still happen in Tokio
//! after the handshake and disconnect the real Owner peer on rejection.
//!
//! A StreamID naming no configured key is a failed guess: it is reported to
//! Tokio (never recorded here, so the Owner takes no write lock) and counts
//! toward the caller IP's ban like any other failed SRT authentication. This
//! relies on srt-rs answering INDUCTION statelessly behind a keyed SYN
//! cookie: a CONCLUSION reaches the resolver only from an address that
//! received this listener's INDUCTION response, so an off-path attacker
//! cannot report failures for (and get banned) someone else's address.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use srt_proto::handshake::{GroupExtensionData, GroupType};
use srt_transport::advanced::admission::{
    AdmissionRequest, AdmissionResolution, ListenerAdmissionResolver, RejectionReason,
};
use srt_transport::{GroupConfig, ListenerEncryptionConfig, ListenerPeerPolicy, PolicyOverride};

use crate::security::{IngestSecurityService, RateLimitScope};
use crate::snapshots::ListenerSocketStats;
use crate::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use restream_domain::domain::srt_ingest::ResolvedSrtCrypto;

use super::srt_policy::SrtIngestPolicyStore;

/// One failed SRT key guess, reported from an Owner's resolver to Tokio.
pub(crate) type AuthFailure = (RateLimitScope, IpAddr);

/// Failed guesses queued for Tokio, shared by every Owner. A full queue drops
/// the report and counts it (`authFailuresDropped`).
pub(crate) const AUTH_FAILURE_CAPACITY: usize = 256;

/// The resolver's view of ingest security: a non-blocking ban check and a
/// lossy, bounded report channel for failed guesses.
#[derive(Clone)]
pub(crate) struct AdmissionGuard {
    pub(crate) security: Arc<IngestSecurityService>,
    pub(crate) failures: flume::Sender<AuthFailure>,
    pub(crate) stats: Arc<ListenerSocketStats>,
}

impl AdmissionGuard {
    /// Banned in either SRT scope, as Tokio's authoritative check decides.
    fn banned(&self, ip: IpAddr) -> bool {
        let ip = ip.to_string();
        [RateLimitScope::SrtPublish, RateLimitScope::SrtRead]
            .into_iter()
            .any(|scope| self.security.try_is_ip_banned_for(scope, &ip).is_some())
    }

    fn report(&self, scope: RateLimitScope, ip: IpAddr) {
        if self.failures.try_send((scope, ip)).is_err() {
            self.stats
                .ingress_owner
                .auth_failures_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
impl AdmissionGuard {
    /// Default security, no bans; reports go nowhere (counted as dropped).
    pub(crate) fn detached(stats: Arc<ListenerSocketStats>) -> Self {
        let (failures, _) = flume::bounded(1);
        Self {
            security: Arc::new(IngestSecurityService::new(
                restream_domain::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG,
            )),
            failures,
            stats,
        }
    }
}

/// The identity Restream advertises as the RECEIVING group on every bonded leg
/// this listener answers. It is distinct from any caller's GROUP id (which
/// identifies the caller's group and alone drives inbound grouping), it is
/// application-owned rather than derived from an address, port or socket id,
/// and it is stable for the listener's lifetime. It need not survive a
/// restart: every session dies with the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReceiverGroupId(u32);

impl ReceiverGroupId {
    /// A fresh random identity for one listener lifetime.
    pub(crate) fn generate() -> Self {
        Self(rand::random::<u32>())
    }

    #[cfg(test)]
    pub(crate) fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// The wire group id (carries the SRT group marker).
    pub(crate) fn wire_id(self) -> u32 {
        GroupConfig::new(self.0, GroupType::Broadcast).group_id
    }
}

/// The resolver handed to `Owner::listen_with_resolver`.
pub(crate) fn ingress_resolver(
    store: Arc<SrtIngestPolicyStore>,
    receiver: ReceiverGroupId,
    guard: AdmissionGuard,
) -> ListenerAdmissionResolver {
    ListenerAdmissionResolver::new(move |request| {
        resolve_ingress_policy(&store, receiver, &guard, request)
    })
}

/// Restream's per-StreamID admission policy: mode validation, authorization
/// eligibility, latency, plaintext-or-encrypted selection, key length, and the
/// receiving-group response.
pub(crate) fn resolve_ingress_policy(
    store: &SrtIngestPolicyStore,
    receiver: ReceiverGroupId,
    guard: &AdmissionGuard,
    request: &AdmissionRequest,
) -> AdmissionResolution {
    #[cfg(test)]
    if request.claimed_identity.stream_id.as_deref() == Some(INJECTED_RESOLVER_PANIC_STREAM_ID) {
        std::panic::resume_unwind(Box::new("injected SRT admission resolver panic"));
    }
    resolve_guarded(
        store,
        receiver,
        guard,
        request.peer.ip(),
        request
            .claimed_identity
            .stream_id
            .as_deref()
            .unwrap_or_default(),
        request.handshake.get_group_extension(),
    )
}

/// A banned caller IP is refused before the policy lookup; a StreamID naming
/// no configured key is reported as a failed guess for that IP.
fn resolve_guarded(
    store: &SrtIngestPolicyStore,
    receiver: ReceiverGroupId,
    guard: &AdmissionGuard,
    peer: IpAddr,
    stream_id: &str,
    caller_group: Option<GroupExtensionData>,
) -> AdmissionResolution {
    if guard.banned(peer) {
        return AdmissionResolution::Reject {
            reason: RejectionReason::FORBIDDEN,
        };
    }
    let resolution = resolve_policy(store, receiver, stream_id, caller_group);
    if matches!(
        resolution,
        AdmissionResolution::Reject { reason } if reason == RejectionReason::UNAUTHORIZED
    ) {
        let scope = match parse_srt_stream_id(stream_id).mode {
            SrtConnectionMode::Read => RateLimitScope::SrtRead,
            SrtConnectionMode::Publish => RateLimitScope::SrtPublish,
        };
        guard.report(scope, peer);
    }
    resolution
}

/// The policy decision over the two request facts it depends on: the claimed
/// StreamID and the caller's GROUP extension (`None` for a direct caller).
pub(crate) fn resolve_policy(
    store: &SrtIngestPolicyStore,
    receiver: ReceiverGroupId,
    stream_id: &str,
    caller_group: Option<GroupExtensionData>,
) -> AdmissionResolution {
    let parsed = parse_srt_stream_id(stream_id);
    if parsed.stream_key.is_empty()
        || !matches!(
            parsed.mode,
            SrtConnectionMode::Publish | SrtConnectionMode::Read
        )
    {
        return AdmissionResolution::Reject {
            reason: RejectionReason::BAD_MODE,
        };
    }
    let Some(resolved) = store.resolved_policy(&parsed.stream_key) else {
        return AdmissionResolution::Reject {
            reason: RejectionReason::UNAUTHORIZED,
        };
    };
    let mut policy = ListenerPeerPolicy {
        latency: PolicyOverride::Set(Duration::from_millis(resolved.latency_ms.max(0) as u64)),
        encryption: PolicyOverride::Set(None),
        ..ListenerPeerPolicy::default()
    };
    if let ResolvedSrtCrypto::Encrypted {
        passphrase,
        pbkeylen,
    } = resolved.crypto
    {
        let Some(key_length) = srt_proto::crypto::KeyLength::from_len(pbkeylen as usize) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        let Ok(encryption) = ListenerEncryptionConfig::new(passphrase, key_length) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        policy.encryption = PolicyOverride::Set(Some(encryption));
    }
    // A caller that sent no GROUP is a direct caller: it gets no receiving
    // group. A bonded caller gets Restream's receiving identity in ITS mode;
    // the caller's own group id is never echoed.
    if let Some(caller_group) = caller_group {
        match receiver_group_for(receiver, caller_group) {
            Some(group) => policy.group = PolicyOverride::Set(Some(group)),
            None => {
                return AdmissionResolution::Reject {
                    reason: RejectionReason::BAD_REQUEST,
                };
            }
        }
    }
    AdmissionResolution::Configure(policy)
}

/// The receiving group answering `caller_group`, in the caller's delivery
/// mode. Only Broadcast and Backup bonds are accepted for ingest.
fn receiver_group_for(
    receiver: ReceiverGroupId,
    caller_group: GroupExtensionData,
) -> Option<GroupConfig> {
    match caller_group.group_type {
        mode @ (GroupType::Broadcast | GroupType::Backup) => {
            Some(GroupConfig::new(receiver.0, mode))
        }
        _ => None,
    }
}

/// A StreamID whose admission panics on the Owner thread, outside the
/// per-peer media boundary (srt-rs runs the resolver inside `Owner::service`).
#[cfg(test)]
pub(crate) const INJECTED_RESOLVER_PANIC_STREAM_ID: &str =
    "#!::r=restream-test-injected-resolver-panic,m=publish";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srt::SrtIngestPolicyEntry;
    use restream_domain::domain::srt_ingest::{SrtGlobalIngestConfig, SrtPipelineIngestConfig};
    use srt_proto::handshake::SRTGROUP_MASK;

    const RECEIVER: u32 = 0x00C0_FFEE;
    const CALLER_GROUP: u32 = 0x0000_1234;

    fn store() -> Arc<SrtIngestPolicyStore> {
        Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[SrtIngestPolicyEntry::new(
                "pipeline-1",
                "live-key",
                SrtPipelineIngestConfig::default(),
            )],
        ))
    }

    /// (stream id, the caller's GROUP extension when it is a bonded caller).
    fn request(stream_id: &str, group: Option<GroupType>) -> (String, Option<GroupExtensionData>) {
        (
            stream_id.to_owned(),
            group.map(|group_type| GroupExtensionData {
                group_id: SRTGROUP_MASK | CALLER_GROUP,
                group_type,
                flags: 0,
                weight: 1,
            }),
        )
    }

    fn resolve(
        store: &SrtIngestPolicyStore,
        receiver: ReceiverGroupId,
        (stream_id, group): (String, Option<GroupExtensionData>),
    ) -> AdmissionResolution {
        resolve_policy(store, receiver, &stream_id, group)
    }

    fn group_of(resolution: AdmissionResolution) -> Option<GroupConfig> {
        match resolution {
            AdmissionResolution::Configure(policy) => match policy.group {
                PolicyOverride::Set(group) => group,
                PolicyOverride::Inherit => None,
            },
            other => panic!("expected Configure, got {other:?}"),
        }
    }

    #[test]
    fn direct_request_configures_no_response_group() {
        let store = store();
        let resolution = resolve(
            &store,
            ReceiverGroupId::from_raw(RECEIVER),
            request("live-key", None),
        );
        assert_eq!(group_of(resolution), None);
    }

    #[test]
    fn broadcast_and_backup_requests_get_the_receiver_id_in_their_mode() {
        let store = store();
        let receiver = ReceiverGroupId::from_raw(RECEIVER);
        for mode in [GroupType::Broadcast, GroupType::Backup] {
            let group = group_of(resolve(&store, receiver, request("live-key", Some(mode))))
                .expect("bonded request gets a receiving group");
            assert_eq!(group.group_id, receiver.wire_id());
            assert_eq!(group.group_type, mode);
            assert_ne!(
                group.group_id,
                SRTGROUP_MASK | CALLER_GROUP,
                "the caller's group id is never echoed"
            );
        }
    }

    #[test]
    fn one_listener_lifetime_advertises_one_receiver_id() {
        let store = store();
        let receiver = ReceiverGroupId::generate();
        let first = group_of(resolve(
            &store,
            receiver,
            request("live-key", Some(GroupType::Broadcast)),
        ))
        .unwrap();
        let second = group_of(resolve(
            &store,
            receiver,
            request("live-key", Some(GroupType::Backup)),
        ))
        .unwrap();
        assert_eq!(first.group_id, second.group_id);
    }

    #[test]
    fn unsupported_group_mode_and_unknown_streams_are_rejected() {
        let store = store();
        let receiver = ReceiverGroupId::from_raw(RECEIVER);
        assert!(matches!(
            resolve(
                &store,
                receiver,
                request("live-key", Some(GroupType::Unknown(3)))
            ),
            AdmissionResolution::Reject { reason } if reason == RejectionReason::BAD_REQUEST
        ));
        assert!(matches!(
            resolve(&store, receiver, request("unknown-key", None)),
            AdmissionResolution::Reject { reason } if reason == RejectionReason::UNAUTHORIZED
        ));
        assert!(matches!(
            resolve(&store, receiver, request("", None)),
            AdmissionResolution::Reject { reason } if reason == RejectionReason::BAD_MODE
        ));
    }

    fn guard(
        failure_limit: i64,
        capacity: usize,
    ) -> (AdmissionGuard, flume::Receiver<AuthFailure>) {
        let mut config = restream_domain::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
        config.failure_limit = failure_limit;
        let (failures, received) = flume::bounded(capacity);
        let guard = AdmissionGuard {
            security: Arc::new(IngestSecurityService::new(config)),
            failures,
            stats: Arc::new(ListenerSocketStats::default()),
        };
        (guard, received)
    }

    fn guarded(guard: &AdmissionGuard, peer: IpAddr, stream_id: &str) -> AdmissionResolution {
        resolve_guarded(
            &store(),
            ReceiverGroupId::from_raw(RECEIVER),
            guard,
            peer,
            stream_id,
            None,
        )
    }

    fn rejected_with(resolution: &AdmissionResolution, expected: RejectionReason) -> bool {
        matches!(resolution, AdmissionResolution::Reject { reason } if *reason == expected)
    }

    /// Unknown stream keys count toward the caller's ban once Tokio records
    /// them, and a banned caller is refused before any policy lookup, even
    /// with a valid key.
    #[test]
    fn failed_key_guesses_ban_the_caller_ip() {
        let (guard, received) = guard(3, AUTH_FAILURE_CAPACITY);
        let attacker: IpAddr = "192.0.2.7".parse().unwrap();
        let bystander: IpAddr = "192.0.2.8".parse().unwrap();
        for guess in ["guess-1", "guess-2", "guess-3"] {
            assert!(rejected_with(
                &guarded(&guard, attacker, guess),
                RejectionReason::UNAUTHORIZED
            ));
        }
        let reported: Vec<_> = received.try_iter().collect();
        assert_eq!(reported.len(), 3, "every unknown key is one failed guess");
        assert!(
            reported.iter().all(
                |report| matches!(report, (RateLimitScope::SrtPublish, ip) if *ip == attacker)
            )
        );
        // Tokio's side of the channel.
        let banned = reported
            .into_iter()
            .map(|(scope, ip)| guard.security.record_failure_for(scope, &ip.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(banned, [false, false, true]);

        assert!(rejected_with(
            &guarded(&guard, attacker, "live-key"),
            RejectionReason::FORBIDDEN
        ));
        assert!(
            received.is_empty(),
            "a refused banned caller is not re-reported"
        );
        assert!(matches!(
            guarded(&guard, bystander, "live-key"),
            AdmissionResolution::Configure(_)
        ));
        // A valid key or a malformed request is not a guess.
        assert!(rejected_with(
            &guarded(&guard, bystander, ""),
            RejectionReason::BAD_MODE
        ));
        assert!(received.is_empty());
        // A read-mode guess counts in the read scope.
        assert!(rejected_with(
            &guarded(&guard, bystander, "#!::r=guess,m=request"),
            RejectionReason::UNAUTHORIZED
        ));
        assert!(matches!(
            received.try_recv(),
            Ok((RateLimitScope::SrtRead, ip)) if ip == bystander
        ));
    }

    /// A full report channel drops the report (counted) rather than block
    /// the Owner thread; the rejection itself is unchanged.
    #[test]
    fn a_full_report_channel_counts_the_dropped_guess() {
        let (guard, received) = guard(3, 1);
        let attacker: IpAddr = "192.0.2.7".parse().unwrap();
        for _ in 0..3 {
            assert!(rejected_with(
                &guarded(&guard, attacker, "guess"),
                RejectionReason::UNAUTHORIZED
            ));
        }
        assert_eq!(received.len(), 1);
        assert_eq!(
            guard
                .stats
                .ingress_owner
                .auth_failures_dropped
                .load(Ordering::Relaxed),
            2
        );
    }
}
