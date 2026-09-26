#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$ROOT_DIR"

echo "[fixture-discipline] validating checked-in fixture contract"
scripts/build/resource-limit.sh cargo test --test fixtures -- --nocapture

declare -a SCAN_ROOTS=(
  "src"
  "tests"
  "test"
  "benches"
)
declare -a INLINE_GENERATOR_PATTERNS=(
  'lavfi'
  'testsrc'
  'testsrc2'
  'smptebars'
  'mandelbrot'
  'anullsrc'
  'anoisesrc'
  'sine='
)

echo "[fixture-discipline] scanning test and benchmark code for inline media generators"
scan_status=0
python3 - "${#SCAN_ROOTS[@]}" "${SCAN_ROOTS[@]}" "${INLINE_GENERATOR_PATTERNS[@]}" <<'PY' || scan_status=$?
import os
import re
import subprocess
import sys

try:
    root_count = int(sys.argv[1])
    roots = sys.argv[2 : 2 + root_count]
    patterns = sys.argv[2 + root_count :]
    tracked_paths = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            *roots,
        ],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    generator = re.compile(
        "|".join(f"(?:{re.escape(pattern)})" for pattern in patterns),
        re.IGNORECASE,
    )
    matches = []
    for raw_path in tracked_paths.split(b"\0"):
        if not raw_path:
            continue
        path = os.fsdecode(raw_path)
        if os.path.islink(path) or not os.path.isfile(path):
            continue
        with open(path, "rb") as source:
            if b"\0" in source.read(4096):
                continue
            source.seek(0)
            for number, raw_line in enumerate(source, 1):
                line = raw_line.decode("utf-8", errors="replace").rstrip("\r\n")
                if generator.search(line):
                    matches.append((path, number, line))
    for path, number, line in matches:
        print(f"{path}:{number}:{line}")
    sys.exit(1 if matches else 0)
except (OSError, subprocess.CalledProcessError, ValueError) as error:
    print(f"[fixture-discipline] scanner failed: {error}", file=sys.stderr)
    sys.exit(2)
PY

if [[ "$scan_status" -eq 1 ]]; then
  cat >&2 <<'EOF'
[fixture-discipline] inline generator patterns found in test-facing code.
Use checked-in assets from test/fixtures/ via src/test_fixtures.rs.
If a case truly cannot be covered by an existing asset, add a dedicated fixture
generation workflow and document why the committed fixture set is insufficient.
EOF
  exit 1
elif [[ "$scan_status" -ne 0 ]]; then
  echo "[fixture-discipline] scanner exited with status $scan_status" >&2
  exit "$scan_status"
fi

echo "[fixture-discipline] passed"
