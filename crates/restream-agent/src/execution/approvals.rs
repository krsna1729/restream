//! Who may approve each change of an operation. Every change is approved on
//! its own by someone whose role reaches the change's minimum; the operation
//! is approved once every change is.

use restream_domain::domain::role::Role;
use serde::Serialize;

/// The least role that may approve a change of `kind`. Removing a
/// destination cannot be undone (its URL and stream key go with it), so it
/// needs an admin; every other change needs an operator, as its direct
/// route does.
pub fn change_role(kind: &str) -> Role {
    match kind {
        "removeOutput" => Role::Admin,
        _ => Role::Operator,
    }
}

/// Who approves: the dashboard session's member and the role it acts as
/// (its effective role, so viewing as a lower role approves as that role).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approver {
    pub username: String,
    pub role: Role,
}

/// One change's approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionApproval {
    pub approved_by: String,
    pub role: Role,
    pub reason: Option<String>,
    pub approved_at: String,
}

#[cfg(test)]
mod tests {
    use super::super::tests::{test_plan, test_request};
    use super::super::{AgentExecutionStore, OperationStatus, OperationStoreError};
    use super::*;
    use restream_agent_contract::ApprovalRequest;

    fn approver(username: &str, role: Role) -> Approver {
        Approver {
            username: username.to_string(),
            role,
        }
    }

    /// An operation of these change kinds, awaiting approval.
    fn operation(store: &AgentExecutionStore, kinds: &[&str]) -> String {
        let mut request = test_request(None);
        let change = request.proposed_changes[0].clone();
        request.proposed_changes = kinds
            .iter()
            .map(|kind| restream_agent_contract::ProposedChange {
                kind: kind.to_string(),
                ..change.clone()
            })
            .collect();
        let created = store.create(request, test_plan()).unwrap();
        created.operation["operationId"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_change_is_approved_only_from_its_minimum_role_up() {
        let store = AgentExecutionStore::default();
        let id = operation(&store, &["removeOutput"]);

        let refused = store.approve_action(&id, 0, &approver("ops", Role::Operator), None);
        assert_eq!(
            refused.unwrap_err(),
            OperationStoreError::RoleTooLow {
                needed: Role::Admin
            }
        );
        let record = store.get(&id).unwrap();
        assert_eq!(record.status, OperationStatus::AwaitingApproval);
        assert_eq!(record.action_approvals, vec![None]);

        let approved = store
            .approve_action(
                &id,
                0,
                &approver("boss", Role::Admin),
                Some("cleanup".into()),
            )
            .unwrap();
        assert_eq!(approved.status, OperationStatus::Approved);
        let given = approved.action_approvals[0].as_ref().unwrap();
        assert_eq!(
            (given.approved_by.as_str(), given.role),
            ("boss", Role::Admin)
        );
        assert_eq!(given.reason.as_deref(), Some("cleanup"));
    }

    #[test]
    fn a_viewer_approves_nothing() {
        let store = AgentExecutionStore::default();
        let id = operation(&store, &["startOutput"]);
        assert_eq!(
            store
                .approve_action(&id, 0, &approver("guest", Role::Viewer), None)
                .unwrap_err(),
            OperationStoreError::RoleTooLow {
                needed: Role::Operator
            }
        );
    }

    #[test]
    fn the_operation_is_approved_when_its_last_change_is() {
        let store = AgentExecutionStore::default();
        let id = operation(&store, &["stopOutput", "removeOutput"]);

        let first = store
            .approve_action(&id, 0, &approver("ops", Role::Operator), None)
            .unwrap();
        assert_eq!(first.status, OperationStatus::AwaitingApproval);
        assert!(first.approval.is_none());

        let last = store
            .approve_action(&id, 1, &approver("boss", Role::Admin), None)
            .unwrap();
        assert_eq!(last.status, OperationStatus::Approved);
        assert_eq!(last.approval.unwrap().approved_by, "boss");
        assert_eq!(
            last.action_approvals[0].as_ref().unwrap().approved_by,
            "ops"
        );
    }

    #[test]
    fn approving_the_whole_operation_needs_its_most_demanding_role() {
        let store = AgentExecutionStore::default();
        let id = operation(&store, &["stopOutput", "removeOutput"]);
        let request = || ApprovalRequest {
            approved_by: "claimed".into(),
            reason: None,
        };

        assert_eq!(
            store
                .approve(&id, &approver("ops", Role::Operator), request())
                .unwrap_err(),
            OperationStoreError::RoleTooLow {
                needed: Role::Admin
            }
        );
        assert_eq!(store.get(&id).unwrap().action_approvals, vec![None, None]);

        let approved = store
            .approve(&id, &approver("boss", Role::Admin), request())
            .unwrap();
        assert_eq!(approved.status, OperationStatus::Approved);
        assert!(approved.action_approvals.iter().all(|slot| {
            slot.as_ref()
                .is_some_and(|given| given.approved_by == "boss")
        }));
    }

    #[test]
    fn an_index_past_the_changes_is_no_such_action() {
        let store = AgentExecutionStore::default();
        let id = operation(&store, &["startOutput"]);
        assert_eq!(
            store
                .approve_action(&id, 1, &approver("boss", Role::Admin), None)
                .unwrap_err(),
            OperationStoreError::NoSuchAction
        );
    }
}
