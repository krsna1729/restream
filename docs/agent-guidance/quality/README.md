# Autonomous Quality Program

Infrastructure that lets small models work this repo autonomously toward:
**correct (proven) · reliable · resilient · modular · efficient · performant**
— hardened enough to carry the biggest broadcast events.

The design premise: a small model is safe and productive when every step is
(1) small, (2) pre-scoped by a well-formed backlog item, (3) verified by
mechanical gates, and (4) journaled so the next iteration starts informed.
The skills provide the rails; the state files provide the memory.

## Pieces

| Piece | Where | Role |
|---|---|---|
| quality-loop skill | `docs/agent-guidance/skills/quality-loop/` | One iteration: select → execute → verify → journal → commit |
| proof-sweep | `docs/agent-guidance/skills/proof-sweep/` | Correctness proofs (unit/proptest/loom/harness) |
| resilience-sweep | `docs/agent-guidance/skills/resilience-sweep/` | Fault injection, recovery, panic containment |
| modularity-sweep | `docs/agent-guidance/skills/modularity-sweep/` | Layering/boundary moves with stop rules |
| perf-sweep | `docs/agent-guidance/skills/perf-sweep/` | Bench ledger, resource guard, measured optimization |
| backlog-groom | `docs/agent-guidance/skills/backlog-groom/` | Refill/re-prioritize the backlog from repo evidence |
| backlog.md | here | Prioritized, tier-tagged work queue |
| journal.md | here | Append-only iteration log (the loop's memory) |
| baselines.md | here | Durable bench/resource ledger |

Supporting task skills (also usable standalone): `check`, `bench`,
`media-test`, `protocol-test`, `concurrency-proof`, `log-audit`, `respin` —
same location, one directory per skill.

All skill bodies are agent-neutral: the canonical instructions are the
`docs/agent-guidance/skills/<name>/SKILL.md` files. Claude Code registers
them through thin shims in `.claude/skills/<name>/SKILL.md`; agents without
a skill system read the canonical files directly (wired via `AGENTS.md`
§ Autonomous Quality Loops).

## Running it

One verified iteration (any Claude Code session in this repo):

```
/quality-loop
```

Continuous, self-paced (recommended):

```
claude --model sonnet
> /loop /quality-loop
```

Fixed cadence: `/loop 45m /quality-loop`. Overnight grooming on a cheap model:
`claude --model haiku` → `/loop /backlog-groom` (haiku takes only `[haiku]`
items and read-only discovery, enforced by the tier gate in the skill).

### Model guidance (matches AGENTS.md)

- **haiku** — inventories, audits, doc updates, grooming. Cheap fuel that
  keeps the backlog sharp for bigger models.
- **sonnet** — the workhorse: scoped fixes, tests, proofs, measured tweaks.
  Default for unattended loops.
- **opus** — `[opus]` items only when a human is nearby: concurrency
  redesign, hot-path architecture, pooling decisions.

## Host safety (why loops are conservative here)

This is an 8 GB WSL2 host with no swap; static FFmpeg links make concurrent
heavy builds a kernel-panic risk. Therefore, and non-negotiably:

- **One quality loop per host.** For parallel agents, use
  `scripts/agent-worktree.sh` and export
  `RESTREAM_BUILD_LOCK_FILE=/tmp/restream-build.lock` so the build lock is
  host-global, not per-worktree.
- Loops never kill media processes they didn't start; they skip the iteration
  instead (a human may be mid-demo via `/respin`).
- Measurement iterations require an otherwise idle host, so bench items may
  skip repeatedly during busy hours. That is correct behavior, not a bug.

## Reviewing what the loop did

- `git log --oneline --grep "quality("` — every loop commit, one item each.
- `journal.md` — the narrative, including failures and skips (honesty is
  enforced: a FAILED entry with numbers is a valid, useful outcome).
- `backlog.md` § Blocked — where the loop wants human or opus help.

Loops never push; publishing is always a human decision.

## Extending

Add work by writing well-formed items into `backlog.md` (format in
backlog-groom). Add a new dimension by writing a sweep skill with the same
shape — discovery recipe + execution recipe + binding rules — into
`docs/agent-guidance/skills/<name>/SKILL.md`, mapping its tag in the
quality-loop dispatch table, and adding a matching registration shim in
`.claude/skills/<name>/SKILL.md`.
