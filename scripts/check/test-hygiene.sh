#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
LOG_FILE="$(mktemp "${TMPDIR:-/tmp}/restream-test-hygiene.XXXXXX.log")"
trap 'rm -f "$LOG_FILE"' EXIT

cd "$ROOT_DIR"

echo "[test-hygiene] checking Rust formatting with pinned toolchain"
CARGO_TERM_COLOR=never cargo fmt --all --check

echo "[test-hygiene] running Rust test graph with captured output"
if ! CARGO_TERM_COLOR=never scripts/build/resource-limit.sh cargo test --workspace -- --nocapture \
  2>&1 | tee "$LOG_FILE"; then
  echo "[test-hygiene] cargo test failed before noise scan" >&2
  exit 1
fi

declare -a NOISE_PATTERNS=(
  'warning:'
  'panicked at'
  'proptest:'
  'failed to find lib.rs or main.rs'
  'specified frame type is not compatible with max B-frames'
  'Could not find codec parameters'
  'not enough frames to estimate rate'
  'ensure_ffmpeg_extracted\(\) must be called before ffmpeg_bin_path\(\)'
  '405 Method Not Allowed'
  'Blocking waiting for file lock on build directory'
  'resource-limit: waiting for another build to finish'
)

echo "[test-hygiene] scanning passing log for known noisy patterns"
scan_status=0
python3 - "$LOG_FILE" "${NOISE_PATTERNS[@]}" <<'PY' || scan_status=$?
import re
import sys

try:
    pattern = re.compile("|".join(f"(?:{item})" for item in sys.argv[2:]))
    with open(sys.argv[1], encoding="utf-8", errors="replace") as log:
        matches = [(number, line.rstrip("\n")) for number, line in enumerate(log, 1) if pattern.search(line)]
except Exception as error:
    print(f"[test-hygiene] noise scanner failed: {error}", file=sys.stderr)
    sys.exit(2)

for number, line in matches:
    print(f"{number}:{line}")
sys.exit(1 if matches else 0)
PY

if [[ "$scan_status" -eq 1 ]]; then
  cat >&2 <<'EOF'
[test-hygiene] noisy output detected in a passing test run.
Quiet the helper or test harness at the source instead of teaching CI to ignore it.
EOF
  exit 1
elif [[ "$scan_status" -ne 0 ]]; then
  echo "[test-hygiene] noise scanner exited with status $scan_status" >&2
  exit "$scan_status"
fi

echo "[test-hygiene] passed"
