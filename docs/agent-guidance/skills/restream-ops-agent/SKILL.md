---
name: restream-ops-agent
description: Investigate or operate live restream pipelines through the agent plane, including validated plans, recorded operation approval, apply, and verification.
---

# Restream Ops Agent

Use for platform operations; application-code edits do not need this workflow.
Prefer `/api/v1/agent/*` and its redacted state over raw control-plane mutations.

1. Read capabilities and context. For incidents, run the investigation workflow
   and report evidence before proposing a change.
2. For a requested change, create and validate a structured plan. Correct
   validation errors within the user's intent; do not apply an invalid plan.
3. When execution is available, prepare the concrete operation and explain its
   graph/runtime impact. The platform requires recorded approval before apply.
   Use approval already granted for that operation; request it only if missing.
4. Apply the approved operation and verify it. After an uncertain response,
   read operation status before retrying; preserve its idempotency key.

Report whether the change is persisted, stopped, running, or waiting on a
runtime condition. `pendingInput` means configuration is present but live
activation needs ingest. Default new outputs to `desiredState=stopped`
unless the user requests live start.

Read [the tool contract](references/tool-contract.md) for route/payload
discovery and verification meanings. A skill does not grant approval on the
user's behalf or bypass the platform's approval state.
