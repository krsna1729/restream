# Documentation archive

Dated investigations, completed migration plans, frozen UI redesign
experiments, and rotated quality ledgers live here so the maintained
documentation set stays short. These files remain linked for evidence and Git
history; they are not prerequisites for normal development or operation.

Current guidance stays under [`docs/`](../README.md). Active quality-program
state (`backlog`, `journal`, `baselines`) stays under
[`docs/agent-guidance/quality/`](../agent-guidance/quality/README.md). Active
UI redesign contracts stay under [`docs/ui-redesign/`](../ui-redesign/brief.md).

## Contents

- [Egress](#egress)
- [Quality evidence](#quality-evidence)
- [UI redesign experiments](#ui-redesign-experiments)
- [Audits](#audits)
- [Archive rules](#archive-rules)

## Egress

- [Egress implementation (completed migration plan)](egress/implementation.md)

The normative live contract is [`docs/egress-architecture.md`](../egress-architecture.md).

## Quality evidence

- [Quality loop journal — 2026-07](quality/journal-2026-07.md)
- [Baselines dated campaigns — 2026-07](quality/baselines-campaigns-2026-07.md)
- [Baselines profiling notes — 2026-07](quality/baselines-profiling-2026-07.md)
- [MSR final report — 2026-07-12](quality/msr-final-report-2026-07-12.md)
- [RTMP egress experiments — 2026-07-13](quality/rtmp-egress-experiments-2026-07-13.md)
- [SRT egress correctness-at-scale investigation — 2026-08-10](quality/srt-egress-scale-investigation-2026-08-10.md)
- [MSR 1,200-output resource attribution — 2026-08-13](quality/msr-1200-resource-attribution-2026-08-13.md)
- [MSR 1,200-output netns confound investigation — 2026-08-14](quality/msr-1200-netns-confound-investigation-2026-08-14.md)
- [SRT fan-in scaling investigation — 2026-08-15](quality/srt-scaling-investigation.md)
- [SRT-RS MSR protocol-mix matrix — 2026-08-25](quality/srt-rs-msr-matrix-2026-08-25.md)
- [SRT-RS upstream work list — 2026-08-25](quality/srt-rs-upstream-worklist-2026-08-25.md)
- [Tokio SRT A/B knobs quiet-host matrix — 2026-09-07](quality/srt-tokio-ab-knobs-2026-09-07.md)

## UI redesign experiments

- [Live MSR operator review — 2026-07-16](ui-redesign/operator-msr-live-review-2026-07-16.md)
- [Overview vertical slice](ui-redesign/overview-slice.md)
- [Component build seam](ui-redesign/build-seam.md)
- [Decision 0001: freeze behavior before framework](ui-redesign/decisions/0001-baseline-before-framework.md)

## Audits

- [Frontend layering audit — 2026-07-21](audits/frontend-layering-audit-2026-07-21.md)

## Archive rules

- Prefer linking archived evidence from current docs with an explicit "dated
  evidence" framing; do not treat archived numbers as current baselines.
- New dated measurements belong in the archive once the active quality ledger
  (`baselines.md` / `journal.md`) has absorbed the durable conclusion.
- Rotate oversized append-only ledgers (journal) into dated archive files
  rather than editing past entries.
- `node scripts/check/docs.mjs` requires every archive Markdown file to be
  linked from this page, and this page to be linked from `docs/README.md`.
