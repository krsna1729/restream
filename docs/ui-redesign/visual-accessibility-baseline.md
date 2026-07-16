# Visual and Accessibility Baseline

The redesign baseline pins the current seeded Overview at three Chromium
viewports. It is an acceptance contract for future UI slices, not a claim that
the current layout is the target design.

| Project | Viewport |
|---|---:|
| `desktop-1440x900` | 1440 x 900 |
| `tablet-1024x768` | 1024 x 768 |
| `mobile-390x844` | 390 x 844 |

Each viewport captures the `empty` and `mixed-health` fixtures and proves that
the page itself does not overflow horizontally. The Overview table may scroll
inside its explicit overflow container on a narrow screen.

The desktop project records the mixed-health Overview's accessible structure,
requires both fixtures to have no serious or critical WCAG 2.0/2.1 A/AA axe
violations,
and proves that the Add Pipeline keyboard path retains focus across a periodic
runtime refresh before Enter opens the dialog and Escape closes it.

These automated checks complement rather than replace manual keyboard, focus,
contrast, zoom, and assistive-technology review.

Run the baseline with:

```sh
npm run test:frontend:redesign-baseline
```

To intentionally refresh committed snapshots after reviewing a UI change:

```sh
npm run test:frontend:redesign-baseline -- --update-snapshots
```

Review every changed PNG and ARIA snapshot. A snapshot update is acceptable
only when the operator state remains legible, the primary action remains
reachable, and the visual or semantic change is intentional. Axe findings are
not snapshotted: serious or critical findings fail directly.
