# Testing Artifacts (Saved During Conversation)

This folder stores reusable test artifacts captured from ad-hoc testing sessions.

## Saved Artifacts

- setup-4x3-copy.sh
- start-inputs-from-manifest.sh
- start-outputs-from-manifest.sh
- health-snapshot.sh
- capture-dashboard-screenshots.mjs
- run-4x3-capture.sh
- session-2026-04-10-4x3.json

## What Each Script Does

1. setup-4x3-copy.sh
- Creates 4 stream keys and 4 pipelines.
- Creates 3 outputs per pipeline (12 outputs total).
- Forces output encoding to copy.
- Writes a run manifest to test/artifacts/session-4x3-last.json by default.

2. start-inputs-from-manifest.sh
- Starts one ffmpeg tee publisher that pushes to all stream keys from a manifest.
- Uses test/colorbar-timer.mp4 in loop mode.
- Stores logs and PID files in test/artifacts/logs.

3. start-outputs-from-manifest.sh
- Starts all outputs listed in a manifest via REST API.
- Accepts 200, 201, and 409 as non-fatal responses.

4. health-snapshot.sh
- Captures /health payload to test/artifacts/runs.
- Prints quick ON counts for input/output.

5. capture-dashboard-screenshots.mjs
- Captures dashboard screenshots (PNG) using Playwright.
- Selects each pipeline row and saves a full-page screenshot.
- Writes a `summary.json` with file paths for the run.

6. run-4x3-capture.sh
- Runs full 4x3 test flow end-to-end.
- Waits for all expected inputs and outputs to reach active state.
- Captures health snapshot.
- Captures one screenshot per pipeline before cleanup.
- Prints final screenshot run directory.

7. wait-all-active.sh
- Polls `/health` until all manifest inputs and outputs are `on`.
- Treats output `warning` as active (running but reader correlation pending), so readiness reflects actual running outputs.
- Fails if readiness timeout is reached.

## Replay Flow

1. Start services:
- make up

2. Create setup + manifest:
- bash test/artifacts/setup-4x3-copy.sh

3. Start input publishers:
- bash test/artifacts/start-inputs-from-manifest.sh test/artifacts/session-4x3-last.json

4. Start outputs:
- bash test/artifacts/start-outputs-from-manifest.sh test/artifacts/session-4x3-last.json

5. Capture health snapshot:
- bash test/artifacts/health-snapshot.sh

## One-Command Make Flow

After app + media services are up, run:
- make run-4x3

## Browser Screenshot Capture

One-time setup:
- npm i -D playwright
- npx playwright install chromium

Capture one screenshot per pipeline:
- node test/artifacts/capture-dashboard-screenshots.mjs

Output:
- PNG screenshots are written to test/artifacts/runs/screenshots/dashboard-screenshots-<timestamp>/.
- `summary.json` is written alongside each screenshot set.

## Notes

- session-2026-04-10-4x3.json is the exact pipeline/output mapping captured from the live run in this conversation.
- session-4x3-last.json is generated each time setup-4x3-copy.sh runs.
