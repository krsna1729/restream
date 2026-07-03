---
name: quality-loop
description: Run ONE bounded, verified iteration of the autonomous quality program — pick the next open backlog item matching your model tier, execute it with the matching sweep skill, verify with the required gates, journal the result, and commit only the item's files. Use when asked to "run the quality loop", "work the backlog", "keep hardening the project", or when invoked repeatedly via /loop.
---

# Skill: quality-loop

One invocation = **one backlog item**, taken from selection to verified commit
(or to a clean, journaled failure). Never more. The loop harness (`/loop`) or a
scheduler provides repetition; this skill provides one safe, auditable step.

## Mission

Drive this project toward: **correct (proven), reliable, resilient, modular,
efficient, performant** — able to carry the biggest broadcast events in history.
Every iteration must leave the repo strictly better and never worse: verified
green gates, an honest journal entry, and no collateral edits.

## State files (the loop's memory)

- `docs/agent-guidance/quality/backlog.md` — prioritized work items
- `docs/agent-guidance/quality/journal.md` — append-only iteration log
- `docs/agent-guidance/quality/baselines.md` — benchmark/resource ledger
- `docs/agent-guidance/quality/README.md` — operator manual (humans)

## Hard safety rules (read every iteration, no exceptions)

1. **Never run `cargo build/test/clippy/check/bench` while restream, mediamtx,
   or ffmpeg are running.** This 8 GB WSL2 host kernel-panics on memory
   pressure. Preflight check: `pgrep -x restream; pgrep -x mediamtx; pgrep -x ffmpeg`.
   If any are running and you did not start them, **skip the iteration**
   (journal `SKIPPED: host busy`) — do not kill processes you don't own.
2. Prefix every heavy command with `scripts/resource-limit`.
3. Never use `--release`; use the default profile for tests, `--profile bench`
   for benchmarks.
4. Never `git push`. Never rewrite history. Never touch files outside your item.
5. Preserve other agents' in-flight work: if `git status` shows modifications
   you didn't make, leave them unstaged and use hunk-based `git add -p`-style
   staging (or explicit file paths) for your own edits only.
6. Never delete or weaken an existing test, gate, or assertion to make
   something pass. If a gate seems wrong, mark the item `blocked` and journal it.
7. Measurement work (benches, resource sweeps) must be serial: nothing else
   building or running on the host.

## Model tier gate

Backlog items carry a tier tag. Attempt only items at or below your tier:

- Haiku-class → `[haiku]` only (read-only audits, inventories, doc updates)
- Sonnet-class → `[haiku]` and `[sonnet]` (scoped fixes, tests, proofs)
- Opus-class or above → any, including `[opus]` (concurrency redesign,
  hot-path architecture, benchmark-driven decisions)

If the top item is above your tier, skip it (leave it open) and take the next
eligible one. Never "just try" an above-tier item.

## Iteration protocol

### 1. Preflight

- `git status --short` — note pre-existing modifications (leave them alone).
- Media-process check per hard rule 1.
- Read the last ~40 lines of `journal.md` and all of `backlog.md`.
- If the previous journal entry is an unresolved `FAILED` for an item still
  marked `in-progress`, your first job is to finish cleaning it up (revert
  stray edits, mark it `blocked` with notes) — that is this iteration's work.

### 2. Select

- Pick the highest-priority `open` item eligible for your tier. Priority =
  file order in `backlog.md` (top is most important), but prefer a dimension
  not touched in the last 3 journal entries when priorities tie (rotation
  keeps all six dimensions moving).
- Mark it `in-progress` in `backlog.md` with today's date, and append a
  one-line `STARTED` journal entry. This is the claim; if a competing loop
  already marked it, pick the next item.
- If no eligible item exists, run the `backlog-groom` skill's discovery step
  for the most stale dimension, file 1–3 new items, journal `GROOMED`, and end
  the iteration.

### 3. Execute

Dispatch to the matching sweep skill and follow it exactly:

| Dimension tag | Skill |
|---|---|
| `[proof]` | proof-sweep |
| `[resilience]` | resilience-sweep |
| `[modularity]` | modularity-sweep |
| `[efficiency]` / `[performance]` | perf-sweep |
| `[groom]` | backlog-groom |

Scope discipline: touch only what the item names. If mid-work you discover a
second problem, do **not** fix it — file it as a new backlog item and continue.

### 4. Verify

Run, in order, stopping at first failure:

1. The item's own listed gates (each item names its gates).
2. `cargo fmt --all --check`
3. `scripts/resource-limit cargo clippy -- -D warnings`
4. `scripts/resource-limit cargo test <scoped filter for the touched modules>`
5. Frontend touched? → `npm run test:frontend`. Contract touched? →
   `./scripts/check-api-contract.sh`. Concurrency touched? →
   `bash ./scripts/check-concurrency-proof-fast.sh`.

**Two-strike rule:** if a gate fails, you get one focused fix attempt. If it
fails again, revert your working edits (`git checkout -- <your files only>`,
never files you didn't touch), mark the item `blocked` with a precise note of
what failed and why, journal `FAILED`, and end the iteration.

### 5. Record and commit

- Mark the item `done` in `backlog.md` (keep the entry, add commit hash after
  committing).
- Append the journal entry (format below).
- Stage **only** the files your item touched, plus `backlog.md` and
  `journal.md` (and `baselines.md` if updated).
- Commit: `quality(<dimension>): <item-id> <one-line summary>`
- Do not push.

### 6. Report

End with a short human-readable summary: item taken, what changed, gate
results, follow-ups filed. If running under `/loop`, this is the iteration
report.

## Journal entry format

```
## <YYYY-MM-DD HH:MM> <item-id> <STARTED|DONE|FAILED|SKIPPED|GROOMED> [model-tier]
- What: <one line>
- Gates: <gate → pass/fail, one line>
- Commit: <hash or "none">
- Follow-ups: <new item ids filed, or "none">
- Notes: <anything the next iteration must know; omit if empty>
```

## Absolute stop conditions

End the iteration immediately (journal `SKIPPED` with the reason) if:

- media processes you don't own are running (hard rule 1)
- another loop's `in-progress` claim is fresher than 4 hours
- the working tree has conflicts or a rebase/merge in progress
- an item requires credentials, external services, or a destructive action
- you have already completed one item this invocation

When in doubt, do less: a small verified step beats a large unverified one.
