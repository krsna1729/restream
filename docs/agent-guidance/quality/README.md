# Autonomous Quality Program

The quality loop completes one evidence-backed backlog item per invocation.
[AGENTS.md](../../../AGENTS.md) owns shared safety and verification rules;
skills hold only task-specific procedures.

## Contents

- [Skills and state](#skills-and-state)
- [Running it](#running-it)
- [Host safety](#host-safety)
- [Maintaining the guidance](#maintaining-the-guidance)

## Skills and state

| Skill | Purpose |
|---|---|
| [quality-loop](../skills/quality-loop/SKILL.md) | Select, execute, verify, and journal one backlog item |
| [backlog-groom](../skills/backlog-groom/SKILL.md) | Find and prioritize concrete work |
| [proof-sweep](../skills/proof-sweep/SKILL.md) | Correctness, fault isolation, and recovery proofs |
| [concurrency-proof](../skills/concurrency-proof/SKILL.md) | Synchronization and lifecycle proof gates |
| [layering-audit](../skills/layering-audit/SKILL.md) | Ownership and dependency boundaries |
| [perf-sweep](../skills/perf-sweep/SKILL.md) | Criterion benchmarks, resource checks, and attribution |
| [protocol-test](../skills/protocol-test/SKILL.md) | Live protocol and fault scenarios |
| [respin](../skills/respin/SKILL.md) | Local live demo |
| [restream-ops-agent](../skills/restream-ops-agent/SKILL.md) | Platform operations with recorded approval and verification |

[Backlog](backlog.md) holds prioritized items and active claims;
[journal](journal.md) records outcomes and blockers;
[baselines](baselines.md) preserves measurements and negative results.

The September 2026 audit consolidated 16 skills into these 9:

| Removed skill | Maintained replacement |
|---|---|
| `check`, `media-test` | AGENTS.md command and gate selection |
| `test-guardrails` | AGENTS.md testing rules and [testing guide](../../testing.md) |
| `bench` | `perf-sweep` |
| `resilience-sweep` | `proof-sweep`, with `concurrency-proof` for lifecycle changes |
| `modularity-sweep` | `layering-audit` |
| `log-audit` | [Logging policy and callsite audit](../../logging.md#callsite-rules) |

The removed slash commands no longer register. Their useful constraints remain
in the listed owners; historical journal entries keep their original names.

## Running it

Request one `quality-loop` iteration, or read its canonical file in an agent
without skill discovery. A scheduler may repeat it when the user requests that.
State whether commits are authorized for the run; otherwise each iteration
leaves a verified working diff and journal entry. Loops never push.

Read the item outcome, gate results, and remaining work in the journal. For
runs authorized to commit, `git log --oneline --grep 'quality('` lists deliveries.

Canonical bodies live in `docs/agent-guidance/skills/<name>/SKILL.md`.
Run `scripts/agent/setup-skills.sh` after changes to refresh local Claude Code
shims and prune removed registrations. Worktree setup runs it automatically;
`.claude/` is generated and gitignored.

The active journal is the current source. Older months may be rotated into
`docs/archive/quality/`; read the newest archived tail when the active file has
fewer than three completed iterations. Dated performance investigations live
under `docs/archive/` and are historical evidence until their commands are
rerun.

## Host safety

One quality loop runs per host. Use the worktree helper and its
`.agent-state/setup.env` for isolated caches and the shared build lock.
Respect active claims regardless of age.

Loops skip heavy builds while another task's media processes run and defer
measurements until the host is otherwise idle. They never kill unowned media
processes. A docs-only iteration need not wait for an idle media host.

## Maintaining the guidance

Keep repository facts, fragile commands, acceptance criteria, and task routing.
Remove duplicated rules, stale source inventories, fixed model-version tables,
and mandatory ceremony that does not establish correctness. Read references
only when the task needs their details. Preserve user scope and existing
authorization; do not treat skill selection as permission to mutate or commit.

This follows current [skill-authoring guidance](https://learn.chatgpt.com/docs/build-skills)
and [model guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra)
on focused skills and conflicting instructions. Model capability is assessed
against the task and its proof requirements; legacy backlog tier tags remain
for compatibility.

Add a skill only for a distinct reusable workflow that existing guidance does
not cover. Validate frontmatter, local links, and generated registrations;
run `node scripts/check/docs.mjs`.
