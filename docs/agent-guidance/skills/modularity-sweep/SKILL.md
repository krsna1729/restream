---
name: modularity-sweep
description: Execute ONE layering/boundary improvement — move orchestration to its owner layer, tighten a module boundary, or verify no cross-layer leak crept in. Use for backlog items tagged [modularity], or when asked to improve layering, reduce coupling, or audit module ownership.
---

# Skill: modularity-sweep

Modularity in this repo is measured by ownership clarity, not module count.
One invocation = one boundary move or one audit, following the layering ladder
in `docs/agent-guidance/skills/layering-audit/SKILL.md` (read it first — it is
the canonical guidance and its stop rules bind here).

## Ownership map (violations are the work queue)

Backend: `api` owns validation/auth/response shaping · `application` owns
orchestration and persistence policy · `media` owns runtime state and hot-path
logic · `db` owns raw SQL · `domain` owns typed graph vocabulary · `planner`
owns backend-selection policy.

Frontend: `app` owns composition/bootstrap · `core` owns shared transport,
state, pure helpers · `features` own bounded UI · `history` owns
history-specific state/rendering.

Known cross-layer flows to keep shrinking (`docs/layering-roadmap.md`):
planner reaching into media backend parsing; runtime core emitting API-shaped
JSON; protocol handlers reading raw SQL; config/domain schemas living inside
runtime modules.

## Execution recipe (backlog item in hand)

1. Read the item's target seam plus `docs/layering-roadmap.md` § the relevant
   area and `docs/current-priorities.md` § 1–3.
2. Apply the lightest rung of the layering ladder that fixes the coupling
   (file split → module → visibility tightening → port/interface → crate,
   never jumping rungs).
3. Move code without changing behavior: no signature "improvements", no
   renames beyond what the move requires, no drive-by cleanups.
4. Hot-path modules keep their runtime focus — a boundary move must not add
   indirection (dyn dispatch, extra channel hops, per-packet trait calls) to
   packet-level loops.
5. Gates: `scripts/build/resource-limit.sh cargo test <scoped>` for the touched area;
   `./scripts/check/api-contract.sh` if any frontend/backend contract surface
   moved; `npm run test:frontend` for frontend moves; full standard gates via
   quality-loop.

## Discovery recipe (finding new [modularity] items)

Run ONE probe, file items, move nothing:

- Grep for upward imports: `db` types in `media`, `serde_json::Value` assembly
  inside `src/media/`, raw SQL outside `db`, API view shaping outside
  `api`/view-model modules.
- Check `docs/layering-roadmap.md` for the topmost not-yet-done step; file it
  with the target files listed.
- Largest-file check: list the 5 largest files in `src/` and `web/ts/`;
  a file is an item only if it mixes ownerships, not merely because it is big.

## Stop rules (from the layering audit — binding)

- Stop when the next split would add more indirection than ownership clarity.
- No new crates/packages/top-level folders because a module "feels busy".
- Avoid new modules unless they remove real complexity
  (`docs/current-priorities.md`).
- If two consecutive modularity items in the journal produced only wrapper
  code, file a `[groom]` item to re-evaluate the direction instead of a third.
