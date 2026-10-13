# Test harness DSL

This directory is the new manifest surface for the integration harness.

The Rust harness loads this surface for canonical suites, scenario-backed
matrix runs, and special workflow inventory.

## Contents

- [Ownership split](#ownership-split)
- [Compatibility](#compatibility)

## Ownership split

Rust keeps:

- process/service lifecycle
- API calls and polling
- FFmpeg/FFprobe invocation
- in-process sinks where still required
- artifact writing
- minimal assertion evaluation

Manifests own:

- command-to-workflow dispatch
- suites and selected scenarios
- runtime defaults and environment override mapping
- service topology and readiness probes
- protocol URL templates
- scenario rows and expected media properties
- check parameters, retry windows, thresholds, and bad-log patterns

## Compatibility

`test/harness/modes.json` is the canonical command surface. New entries should
be `kind: suite`, `kind: scenario`, or an explicit special-workflow runner; do
not add deprecated manifest kinds.
