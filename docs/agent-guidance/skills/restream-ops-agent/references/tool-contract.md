# Agent Operation Contract

Read this when constructing agent-plane requests. Discover current tool names,
schemas, and compiled features from `get_agent_capabilities`
(`GET /api/v1/agent/capabilities`).
The [catalog source](../../../../../src/agent_plane/catalog.rs) and
[API reference](../../../../api-reference.md#optional-agent-plane) own the full contract.

## Contents

- [Operation sequence](#operation-sequence)
- [Request shape](#request-shape)
- [Result interpretation](#result-interpretation)

## Operation sequence

Use `get_agent_context` for redacted state and `investigate_pipeline_issue`
for incident evidence. For changes:

`plan_pipeline_change` → `create_agent_operation` →
`approve_agent_operation` → `apply_agent_operation` →
`verify_agent_operation`.

The plan returns validation and graph impact; use `validate_change` or
`preview_graph_diff` when only those views are needed. Read existing operation
state with `get_agent_operation` before retrying an uncertain apply.

Recording approval must reflect actual user authorization for the concrete
operation. Creating an operation or requesting a plan does not supply that
approval.

## Request shape

A plan includes `intent`, `pipelineId`, and `proposedChanges`. For example:

```json
{
  "intent": "Attach a stopped local output",
  "pipelineId": "p1",
  "proposedChanges": [{
    "kind": "addOutput",
    "name": "Local sink",
    "url": "rtmp://127.0.0.1:1935/live/demo",
    "config": {
      "video": { "mode": "source" },
      "audio": { "mode": "all" }
    },
    "desiredState": "stopped"
  }]
}
```

Operation creation uses the same change payload plus an `idempotencyKey`
unique to that intended request. Preserve it across retries; do not reuse it
for a different change. Supply actor/tool identity fields according to the
current schema. Obtain supported change kinds from capabilities.

## Result interpretation

- Invalid validation prevents apply; correct the plan within the requested scope.
- `executionEnabled=false` leaves planning available; execution may be compiled out.
- `approvalRequired=true` prevents apply until approval is recorded.
- `pendingInput` means persistence succeeded but ingest is needed for live activation.
- `stopped` satisfies a requested stopped state.
- Verify running state and report any remaining convergence or health failure.
