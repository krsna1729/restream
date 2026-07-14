# Operator Task Model

## Observe

An operator must be able to:

- tell whether Restream itself is reachable, ready, degraded, or recovering;
- scan all pipelines and identify the upstream cause of a degraded path;
- distinguish configured intent from current runtime state;
- recognize stale data and understand whether SSE or polling is providing it;
- inspect input, output, recording, file-ingest, and HLS-preview state;
- identify retry attempts, backoff, recent failures, and unexpected stops;
- compare engine and pipeline telemetry without changing runtime state.

## Navigate

An operator must be able to:

- open Overview, Pipelines, Media, Settings, and Status directly;
- preserve a selected pipeline while moving between Operate, Inspect, and
  Monitor;
- follow legacy `mode=inspect`, `mode=control`, and `mode=admin` links;
- use browser back and forward navigation without losing canonical location;
- complete the primary path with keyboard-only navigation at desktop and
  mobile widths.

## Diagnose

An operator must be able to:

- locate a degraded pipeline and the affected output;
- open diagnostics or the processing graph without changing desired state;
- inspect recent lifecycle evidence without exposing raw secrets;
- distinguish input loss from an isolated downstream output failure;
- recover context after an SSE disconnect or hidden-tab interval.

## Operate

An operator must be able to:

- start or stop an output and see pending intent until runtime convergence;
- start or stop recording and file ingest with the same intent/runtime split;
- create, edit, or delete pipelines and outputs with explicit confirmation;
- understand slow, failed, rejected, or expired-authentication mutations;
- avoid duplicate actions while a mutation is already in flight.

## Safety rules

- Destructive actions remain explicit and keyboard reachable.
- A generic spinner must not replace meaningful mutation state.
- Synthetic fixtures use `.invalid` hosts and obviously fake identifiers.
- Screenshots and design captures must not include stream keys, credentials,
  destination secrets, raw diagnostics, or unredacted operational history.
- A visual match is not acceptance evidence without behavioral and
  accessibility proofs.
