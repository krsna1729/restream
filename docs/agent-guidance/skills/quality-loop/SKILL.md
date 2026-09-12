---
name: quality-loop
description: Execute one bounded quality-backlog iteration when the user requests backlog work or an autonomous quality loop.
---

# Quality Loop

One invocation handles one backlog item, including verification and a journal
entry. A scheduler or explicit user request controls repetition. Ordinary
coding, reviews, and checks do not start this workflow.

State lives in [backlog](../../quality/backlog.md),
[journal](../../quality/journal.md), and [baselines](../../quality/baselines.md).
Use [AGENTS.md](../../../../AGENTS.md) for build safety, capability tiers,
worktree isolation, and gate selection.

Read the active journal first. If it has fewer than three completed entries,
read the newest archived journal tails under `docs/archive/quality/` as needed
for selection, failure recovery, and recent-dimension rotation. Do not copy
archived entries back into the active file.

## Select and execute

1. Check the working tree and recent journal entries. Respect another loop's
   active claim; elapsed time alone does not release it. Resolve the ownership
   of an interrupted iteration before touching its diff.
2. Pick the highest-priority open item within the available model's capability.
   Mark it `in-progress` and journal `STARTED`. If none is eligible, use
   [backlog-groom](../backlog-groom/SKILL.md) for evidence-backed discovery and
   end this iteration.
3. Read only the guidance the item requires:

   | Dimension | Guidance |
   |---|---|
   | proof / resilience | [proof-sweep](../proof-sweep/SKILL.md) |
   | modularity | [layering-audit](../layering-audit/SKILL.md) |
   | efficiency / performance | [perf-sweep](../perf-sweep/SKILL.md) |
   | groom | [backlog-groom](../backlog-groom/SKILL.md) |

4. Complete the item's observable goal. File separate discoveries without
   expanding the current item.
5. Run its acceptance gates and the applicable AGENTS.md gates, starting
   narrow. Do not repeat a passing gate unless a new change or unresolved
   concern justifies it. Fix relevant failures; do not weaken assertions to
   obtain a pass.

## Finish or hand off

Mark `done` only when the goal and gates pass. Journal the result and leave a
reviewable diff. Commit only when the user has authorized commits for this run;
then stage only the item's own hunks and use
`quality(<dimension>): <item-id> <summary>`. Never push from the loop.

If progress needs unavailable resources, ownership resolution, or work beyond
the item's scope, record the blocker and verification still needed. Preserve
useful work with clear state; remove failed experimental edits only with an
inverse patch to your own hunks. Do not erase another iteration's work.

Skip an iteration that requires a heavy build while unowned media processes
are running, or measurement while the host is busy. Do not kill those processes.
Do not start an item during an unresolved merge/rebase or another active loop.

Journal entries record timestamp, item ID, outcome
(`STARTED|DONE|FAILED|SKIPPED|GROOMED`), capability tier, what changed, gate
results, commit (or `none`), and follow-ups. If a commit is created, record its
hash afterward without amending/recreating the commit just to embed its own hash.
