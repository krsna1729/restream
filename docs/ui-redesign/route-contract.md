# Dashboard Route Contract

## Contents

- [Canonical locations](#canonical-locations)
- [Compatibility mappings](#compatibility-mappings)
- [Required behavior](#required-behavior)

The URL is the canonical owner of dashboard location. A redesign must reuse
`web/ts/core/pipeline-workspace.ts` rather than introduce a parallel router.

## Canonical locations

| Location | Canonical query |
|---|---|
| Overview | `?mode=overview` |
| Incidents | `?mode=incidents` |
| Telemetry | `?mode=telemetry` |
| Pipeline Operate | `?mode=pipeline&view=operate&p=<pipeline-id>` |
| Pipeline Inspect | `?mode=pipeline&view=inspect&p=<pipeline-id>` |
| Pipeline Monitor | `?mode=pipeline&view=monitor&p=<pipeline-id>` |
| Media | `?mode=media` |
| Settings | `?mode=settings` |
| Status | `?mode=status` |

`p` is meaningful only in pipeline mode. Non-pipeline locations remove `view`
and `p` while preserving unrelated query parameters.

## Compatibility mappings

| Incoming query | Canonical result |
|---|---|
| `?mode=inspect&p=x` | `?mode=pipeline&view=inspect&p=x` |
| `?mode=control&p=x` | `?mode=pipeline&view=monitor&p=x` |
| `?mode=admin` | `?mode=settings` |
| missing `mode` with `p=x` | `?mode=pipeline&view=operate&p=x` |
| missing `mode` and `p` | `?mode=overview` |

## Required behavior

- Initial resolution uses `replaceState` only when canonicalization is needed.
- Operator navigation uses `pushState` and remains compatible with `popstate`.
- One user navigation intent creates one history step. For example, v2
  Overview's Operate action moves directly to
  `?mode=pipeline&view=operate&p=<pipeline-id>` and its Inspect action moves
  directly to `?mode=pipeline&view=inspect&p=<pipeline-id>` instead of pushing
  an intermediate Overview URL with `p=`.
- Re-selecting the already-active dashboard or pipeline workspace tab is a
  no-op for browser history. This keeps Back focused on real operator movement
  rather than duplicate copies of the same URL.
- A selected pipeline is reconciled by stable ID, name, or persisted hint when
  configuration refreshes replace runtime IDs.
- Invalid or absent pipeline selections remain safely representable.
- Base-path deployment must preserve every query contract above.

Existing proof: `test/frontend/frontend-pipeline-workspace.test.mjs` and
`test/frontend/redesign/seed.spec.ts`.
