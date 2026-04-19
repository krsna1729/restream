# Frontend ES Module Conventions

This guide documents how frontend modules are structured in this repository and how to avoid regressions when refactoring dashboard code.

## 1. Goals

- Make dependencies explicit with import/export.
- Keep runtime state sharing predictable.
- Preserve compatibility with existing HTML-bound handlers.

## 2. Loading Model

Dashboard and stream-key pages load scripts as ES modules via `<script type="module">` in:

- `public/index.html`
- `public/stream-keys.html`

Because files are modules:

- symbols are module-scoped by default
- cross-file usage must be imported explicitly
- implicit global access should be treated as a bug unless intentionally exposed on `window`

## 3. Shared State Contract

Use `public/js/core/state.js` as the single shared mutable state object:

- `state.config`
- `state.health`
- `state.pipelines`
- `state.metrics`

Rules:

- write state in orchestration/fetch paths (mainly dashboard refresh flows)
- read state in render and interaction modules
- do not reintroduce separate global state variables

## 4. Cross-Module Dependency Rules

1. Prefer imports for any normal cross-file dependency.
2. Keep module APIs explicit with named exports.
3. Avoid circular dependencies unless there is no practical alternative.
4. If an HTML attribute invokes a function directly, expose only that function on `window`.

Examples where `window.*` exposure is valid:

- `onclick="selectPipeline(...)"`
- modal open/close actions wired in markup
- data-attribute callbacks expecting global functions

## 5. Troubleshooting Checklist

If a panel disappears, render stops halfway, or controls stop responding after refactor:

1. Check browser console for `ReferenceError` and identify whether symbol should be imported or `window`-exposed.
2. Verify page markup still points to valid handler names for inline attributes.
3. Confirm state reads/writes use `state.*` rather than removed globals.
4. Confirm required functions are still attached to `window` for HTML-bound hooks.
5. Force reload to bypass stale browser cache when testing recent JS changes.

## 6. Quick Verification

After frontend module changes, run:

1. syntax checks for modified module files
2. dashboard load + pipeline selection in browser
3. stream-keys page load and key actions
4. console review for runtime errors

This keeps migration failures visible before commit.
