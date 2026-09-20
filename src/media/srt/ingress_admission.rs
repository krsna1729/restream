//! Synchronous SRT ingress admission policy, resolved on the Owner thread.
//!
//! `srt_transport::compio::Owner::listen_with_resolver` calls this after cookie
//! validation and before CONCLUSION is processed. It reads only the already
//! populated [`SrtIngestPolicyStore`]; the asynchronous control-plane checks
//! (pipeline authentication, IP bans, duplicate publishers) still happen in
//! Tokio after the handshake and disconnect the real Owner peer on rejection.

use std::sync::Arc;
use std::time::Duration;

use srt_proto::handshake::{GroupExtensionData, GroupType};
use srt_transport::advanced::admission::{
    AdmissionRequest, AdmissionResolution, ListenerAdmissionResolver, RejectionReason,
};
use srt_transport::{GroupConfig, ListenerEncryptionConfig, ListenerPeerPolicy, PolicyOverride};

use crate::domain::srt_ingest::ResolvedSrtCrypto;
use crate::media::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};

use super::srt_policy::SrtIngestPolicyStore;

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
) -> ListenerAdmissionResolver {
    ListenerAdmissionResolver::new(move |request| resolve_ingress_policy(&store, receiver, request))
}

/// Restream's per-StreamID admission policy: mode validation, authorization
/// eligibility, latency, plaintext-or-encrypted selection, key length, and the
/// receiving-group response.
pub(crate) fn resolve_ingress_policy(
    store: &SrtIngestPolicyStore,
    receiver: ReceiverGroupId,
    request: &AdmissionRequest,
) -> AdmissionResolution {
    resolve_policy(
        store,
        receiver,
        request
            .claimed_identity
            .stream_id
            .as_deref()
            .unwrap_or_default(),
        request.handshake.get_group_extension(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::srt_ingest::{SrtGlobalIngestConfig, SrtPipelineIngestConfig};
    use crate::media::srt::SrtIngestPolicyEntry;
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
}
