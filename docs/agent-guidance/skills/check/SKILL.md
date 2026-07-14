---
name: check
description: Run the standard pre-commit quality loop for this repo — format check, clippy with warnings denied, then the full test suite. Use before committing, after finishing a change, or when asked to "run checks", "verify the build", or "make sure nothing broke".
---

# Skill: check

Run the standard pre-commit quality loop for this repo: format check, clippy, then tests. Uses the resource-limit wrapper to share build locks with any parallel agents.

## Safety preflight

Never run Cargo alongside a live pipeline on a constrained host:

```sh
pgrep -x restream || pgrep -x mediamtx || pgrep -x ffmpeg
```

If any are running, stop and ask (or, in an autonomous loop, skip the iteration) — do not kill processes you did not start unless the user asked for a rebuild/respin.

## Steps

1. Run `cargo fmt --all --check` — report misformatted files, do NOT auto-fix.
2. Run `scripts/build/resource-limit.sh cargo clippy -- -D warnings` — fail on any warning.
3. Run `scripts/build/resource-limit.sh cargo test` — run the full unit/integration test suite.

Stop at the first failure and report clearly what failed and how to fix it. Do not proceed to the next step if a prior step fails.

If all three pass, report a short summary: "fmt ✓  clippy ✓  tests ✓" and the test count.

## Notes
- Never use `--release`; use `--profile bench` only if explicitly building for benchmarks.
- `cargo fmt` (without `--check`) auto-fixes; only use `--check` here unless the user explicitly asks to format.
- Use `cargo fmt --all` / `cargo fmt --all --check`; do not run `rustfmt` directly.
- The resource limiter uses `RESTREAM_BUILD_LOCK_FILE` when supplied. In an
  agent worktree, source `.agent-state/setup.env` so all worktrees share the
  host-global lock selected by `scripts/agent/worktree.sh`.
- Passing test logs must stay quiet: no warnings, panic text, or FFmpeg probe chatter (see `scripts/check/test-hygiene.sh`).
