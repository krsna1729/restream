---
name: proof-sweep
description: Close ONE correctness-proof gap — add a missing unit/property/loom/harness proof, tighten an invariant, or convert an unproven assumption into a tested one. Use for backlog items tagged [proof], or when asked to improve test coverage, prove an invariant, or audit unwrap/expect/panic paths.
---

# Skill: proof-sweep

Correctness here means **proven**, not "looks right": every invariant in
AGENTS.md § Media Rules should be backed by a test that fails when the
invariant breaks. One invocation closes one gap.

## Proof ladder (pick the lightest layer that catches the bug)

1. **Unit test with synthetic packets** — pure logic, milliseconds. The
   precise oracle (see `docs/testing-strategy.md`).
2. **Property test (proptest is a dev-dependency)** — parser/mux/timestamp
   invariants over generated inputs: DTS monotonicity, FLV composition-offset
   PTS derivation, SRT Stream ID normalization, ring index arithmetic.
3. **Loom model (`scripts/harness/loom-target.sh`)** — wake/cancel and registry
   race orderings. Only for genuine multi-thread interleaving questions.
4. **Live harness mode** — real sockets/processes, only when the property is
   about the running binary (use the concurrency-proof skill's gates).

## Execution recipe (backlog item in hand)

1. Read the target module and the invariant the item names. Read
   `docs/testing.md` § relevant section first.
2. Write the proof so it **fails against a deliberately broken invariant**
   (mutate the condition locally, watch it fail, revert the mutation). A proof
   that can't fail proves nothing — this step is mandatory.
3. Keep passing output quiet (no warnings, panic text, FFmpeg chatter).
4. Use fixtures via `src/test_fixtures.rs`; never generate media inline.
5. If the proof covers a concurrency rule, decide whether
   `scripts/check/concurrency/fast.sh` must be extended so it stays
   mandatory — extending the gate is part of the item.
6. Gates: scoped `cargo test <filter>` + the standard quality-loop gates.

## Discovery recipe (when asked to find new [proof] items)

Run ONE of these probes and file items for the top findings (do not fix
anything during discovery):

- **Coverage map:** `scripts/build/resource-limit.sh cargo llvm-cov --summary-only`
  (cargo-llvm-cov is installed). File one item per weakest `src/media/` module.
  Kill any live pipeline check first; this is a heavy build.
- **Panic-path inventory:** `grep -rn "\.unwrap()\|\.expect(\|panic!\|unreachable!" src/media/ --include="*.rs"`
  excluding `#[cfg(test)]` blocks. Classify each hit: invariant-safe (document
  why in a comment-sized note in the item) vs fallible (file a fix item —
  "no failure path may crash the engine").
- **Invariant × proof cross-check:** for each AGENTS.md § Media Rules bullet,
  name the test that enforces it. Bullets with no test become items.
- **Gate-coverage check:** read `docs/stage-boundary-proof-map.md` and diff its
  current proof claims against `scripts/check/concurrency/fast.sh`;
  uncovered rules become items.

## Rules

- A test that never failed during development is unverified — always do the
  break-it-first check.
- Do not chase 100% line coverage; chase invariant coverage. A module at 60%
  lines with every invariant proven beats 95% lines of incidental assertions.
- Property tests get explicit `proptest!` case counts small enough to stay
  fast in CI (default 256 is usually fine; justify anything larger).
- Loom targets must terminate; bound the model (checkpoints feature is
  enabled) and keep them in the loom cfg so normal builds skip them.
