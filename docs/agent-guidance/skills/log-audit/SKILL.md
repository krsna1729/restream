---
name: log-audit
description: Audit every tracing callsite in src/ for correct log level (error/warn/info/debug), fix mismatches in place, and update the callsite audit table in docs/logging.md. Use when asked to audit logging, fix log levels, or when log noise/severity looks wrong.
---

# Skill: log-audit

Audit every tracing callsite in `src/` for correct log level, fix mismatches,
and update the callsite audit table in `docs/logging.md`.

## Decision rules

**`error!`** — the process cannot continue delivering for this pipeline/output
due to a fault that is *not* the remote client's fault:
- Socket or thread allocation failure
- FFmpeg crash or panic
- DB write failure
- Egress delivery permanently broken (will page on-call)

**`warn!`** — recoverable or client-caused:
- Client sent invalid credentials / stream key
- Remote destination rejected a connection (egress will retry)
- Transient accept/read error (loop continues)
- Resource capacity limit reached (clean rejection, server healthy)
- Configuration advisory (system runs with degraded behaviour)

**`info!`** — lifecycle transitions an operator needs to see:
- Server / listener started
- Ingest connected / disconnected
- Egress started / stopped
- Key stream property discovered (codec, audio tracks)
- Ring reader registered / deregistered

**`debug!`** — internal diagnostics; too frequent or too low-level for ops:
- Fires every reconciler tick or per-connection setup step
- Internal plumbing (ring/queue creation, FFmpeg I/O context)
- Normal shutdown paths (stdout closed, stage swept because idle)

**Quick test:** *Who is at fault and does the task continue?*
- System fault + task stops → `error`
- Client fault or recoverable → `warn`
- State change an operator cares about → `info`
- Implementation detail → `debug`

## Steps

1. Grep all tracing macros across `src/` (exclude `src/bin/`):
   ```sh
   grep -rn "error!\|warn!\|info!\|debug!" src/ --include="*.rs" | grep -v "src/bin/"
   ```

2. For each callsite, apply the decision rules above. Common patterns to watch for:
   - `error!` on per-client errors (invalid key, bad auth, client disconnect) → `warn`
   - `error!` on diagnostic probes (RSS logging, stat sampling) → `debug`
   - `error!` on normal shutdown paths (stdout/pipe closed) → `debug`
   - `info!` on anything that fires every reconciler tick → `debug`

3. Fix misclassified callsites in-place. Keep the message text and structured
   fields unchanged; only change the macro name.

4. Compile-check: `scripts/build/resource-limit.sh cargo check`

5. Update `docs/logging.md` § "Callsite Audit":
   - Add or update the row for each changed callsite.
   - Record the *reasoning*, not just the level. One sentence is enough.
   - If a new module appears, add a subsection for it.

6. Commit: one commit for the callsite fixes, including the doc update.

## Notes

- Do not change message text or structured field names — only the macro level.
- Benchmark output inside `#[cfg(test)]` blocks is exempt; those don't reach
  production subscribers.
- `ring_buffer.rs` and `avio.rs` packet loops must stay logging-free; control
  paths (creation, resize, registration) may use `debug!` or `info!`.
- After fixing, re-read the audit section in `docs/logging.md` to confirm every
  changed callsite has an entry explaining why.
