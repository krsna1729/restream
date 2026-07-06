# Test harness DSL migration

This directory is the new manifest surface for the integration harness.

The current Rust harness can ignore these files until the loader/planner is wired. They are intentionally additive so the migration can proceed in small commits.

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

During migration, modes can stay `kind: legacy`. The new engine only takes over modes converted to `kind: suite` or `kind: scenario`.
