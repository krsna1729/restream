#!/usr/bin/env bash
# Source-wide audit automation to enforce clean layering, prevent growth of
# god files, and catch un-centralized environment variables.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
mkdir -p target

echo "=== Restream Source Audit ==="
FAILED=0

# 1. Check for forbidden imports in media modules
echo "Checking forbidden imports in src/media/..."
FORBIDDEN_IMPORTS=$(grep -rn "use crate::api" src/media/ || true)
if [ -n "$FORBIDDEN_IMPORTS" ]; then
    echo "FAIL: Media modules must not import api types:" >&2
    echo "$FORBIDDEN_IMPORTS" >&2
    FAILED=1
else
    echo "OK: No forbidden imports found."
fi

echo ""
echo "Checking file size limits..."
SOURCE_LINE_LIMIT=2000
LARGE_FILE_REPORT=target/source-audit-large-files.jsonl
: > "$LARGE_FILE_REPORT"
SOURCE_FILE_FIND_ARGS=(
    src
    public/ts
    test
    -path test/artifacts -prune
    -o
    -path test/fixtures -prune
    -o
    -type f
    \( -name '*.rs' -o -name '*.ts' -o -name '*.mjs' -o -name '*.js' \)
    -print0
)
while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file" | tr -d ' ')
    printf '{"file":"%s","lines":%s,"limit":%s}\n' \
        "$file" "$lines" "$SOURCE_LINE_LIMIT" >> "$LARGE_FILE_REPORT"
    if [ "$lines" -gt "$SOURCE_LINE_LIMIT" ]; then
        echo "FAIL: $file has $lines lines (limit: $SOURCE_LINE_LIMIT)" >&2
        FAILED=1
    fi
done < <(find "${SOURCE_FILE_FIND_ARGS[@]}")

LARGEST_FILES=$(
    sort -t: -k2,2nr <(
        while IFS= read -r -d '' file; do
            lines=$(wc -l < "$file" | tr -d ' ')
            printf '%s:%s\n' "$file" "$lines"
        done < <(find "${SOURCE_FILE_FIND_ARGS[@]}")
    ) | head -20
)

if [ "$FAILED" -eq 0 ]; then
    echo "OK: All audited source/test files are at or below ${SOURCE_LINE_LIMIT} lines."
fi

# 3. Check for raw std::env::var usage outside src/config.rs and tests
echo ""
echo "Checking for inline std::env::var usage outside src/config.rs..."
# Exclude config.rs, main.rs (tokio runtime limits), test harness,
# tests, benches, test_fixtures.rs, lib.rs (config-chain helpers),
# planner (BackendPolicy::from_env), and restream-mcp (separate binary).
RAW_ENV_VARS=$(grep -rn "std::env::var" src/ \
    | grep -v "src/config.rs" \
    | grep -v "src/main.rs" \
    | grep -v "src/lib.rs" \
    | grep -v "src/planner/" \
    | grep -v "src/bin/test_harness" \
    | grep -v "src/bin/restream-mcp.rs" \
    | grep -v "tests/" \
    | grep -v "benches/" \
    | grep -v "test_fixtures.rs" || true)

if [ -n "$RAW_ENV_VARS" ]; then
    echo "FAIL: Found raw std::env::var usage outside approved config/test harness owners:" >&2
    echo "$RAW_ENV_VARS" >&2
    FAILED=1
else
    echo "OK: No raw std::env::var usage found outside configuration module."
fi

echo ""
echo "Checking API stage-start guardrails..."
API_MANUAL_STAGE_STARTS=$(grep -REn \
    "Command::new|ensure_ffmpeg|ffmpeg_bin_path|get_or_create_transcoder|get_or_create_h264_transcoder|spawn_ffmpeg|run_.*ffmpeg|external_transcoder" \
    src/api/ || true)
if [ -n "$API_MANUAL_STAGE_STARTS" ]; then
    echo "FAIL: API route/view modules must not manually start FFmpeg/transcoder stages:" >&2
    echo "$API_MANUAL_STAGE_STARTS" >&2
    FAILED=1
else
    echo "OK: API modules do not start FFmpeg/transcoder stages."
fi

echo ""
echo "Checking harness status-schema guardrails..."
HARNESS_STATE_FIELD_READS=$(grep -REn '\["state"\]' src/bin/test_harness/ src/bin/test_harness.rs || true)
if [ -n "$HARNESS_STATE_FIELD_READS" ]; then
    echo "FAIL: Harness reads a non-schema output status field named state:" >&2
    echo "$HARNESS_STATE_FIELD_READS" >&2
    FAILED=1
else
    echo "OK: Harness does not read the removed output status state field."
fi

python3 - "$SOURCE_LINE_LIMIT" <<'PY'
import json
import pathlib
import re
import subprocess
import sys

line_limit = int(sys.argv[1])
root = pathlib.Path(".")

def rel(path: pathlib.Path) -> str:
    return path.as_posix()

def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")

source_files = [
    path
    for base in [root / "src", root / "public" / "ts", root / "test"]
    if base.exists()
    for path in base.rglob("*")
    if path.is_file()
    and path.suffix in {".rs", ".ts", ".mjs", ".js"}
    and "test/artifacts/" not in rel(path)
    and "test/fixtures/" not in rel(path)
]

line_counts = [
    {"file": rel(path), "lines": len(read(path).splitlines()), "limit": line_limit}
    for path in source_files
]
line_counts.sort(key=lambda row: (-row["lines"], row["file"]))

public_function_re = re.compile(r"\bpub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
public_functions = []
for path in sorted((root / "src").rglob("*.rs")):
    names = public_function_re.findall(read(path))
    if names:
        public_functions.append(
            {"module": rel(path), "count": len(names), "functions": sorted(names)}
        )

route_counts = []
for path in sorted((root / "src" / "api").glob("*.rs")):
    body = read(path)
    count = body.count(".route(")
    if count:
        route_counts.append({"module": rel(path), "routes": count})

def load_json(path: pathlib.Path):
    return json.loads(read(path)) if path.exists() else {}

modes = load_json(root / "test" / "harness" / "modes.json")
suites = load_json(root / "test" / "harness" / "suites.json")
mode_rows = []
for group in modes.get("modeGroups", []):
    for name, spec in group.get("modes", {}).items():
        mode_rows.append(
            {
                "name": name,
                "kind": group.get("kind"),
                "group": group.get("group"),
                "suiteRef": spec.get("suiteRef"),
                "suiteDefault": bool(spec.get("suiteDefault", False)),
                "benchProfile": bool(spec.get("requires", {}).get("benchProfile", False)),
                "portNamespace": bool(spec.get("requires", {}).get("portNamespace", False)),
            }
        )
dynamic_modes = modes.get("dynamicModes", [])

env_re = re.compile(r"std::env::var(?:_os)?\(\s*\"([A-Za-z_][A-Za-z0-9_]*)\"")
env_usage = []
for path in sorted((root / "src").rglob("*.rs")):
    for line_no, line in enumerate(read(path).splitlines(), start=1):
        match = env_re.search(line)
        if match:
            env_usage.append({"file": rel(path), "line": line_no, "name": match.group(1)})

media_api_imports = subprocess.run(
    ["grep", "-rn", "use crate::api", "src/media/"],
    text=True,
    capture_output=True,
)
api_stage_starts = subprocess.run(
    [
        "grep",
        "-REn",
        r"Command::new|ensure_ffmpeg|ffmpeg_bin_path|get_or_create_transcoder|get_or_create_h264_transcoder|spawn_ffmpeg|run_.*ffmpeg|external_transcoder",
        "src/api/",
    ],
    text=True,
    capture_output=True,
)
harness_state_reads = subprocess.run(
    ["grep", "-REn", r'\["state"\]', "src/bin/test_harness/", "src/bin/test_harness.rs"],
    text=True,
    capture_output=True,
)

feature_cfg_sites = []
for path in [root / "Cargo.toml", *sorted((root / "src").rglob("*.rs"))]:
    if path.exists():
        for line_no, line in enumerate(read(path).splitlines(), start=1):
            if "#[cfg(feature" in line or "required-features" in line:
                feature_cfg_sites.append({"file": rel(path), "line": line_no, "text": line.strip()})

report = {
    "sourceLineLimit": line_limit,
    "largestFiles": line_counts[:20],
    "lineCounts": line_counts,
    "publicFunctions": public_functions,
    "routeCounts": route_counts,
    "harnessInventory": {
        "commandSurface": modes.get("commandSurface"),
        "defaultCommand": modes.get("defaultCommand"),
        "modeCount": len(mode_rows),
        "modes": sorted(mode_rows, key=lambda row: row["name"]),
        "dynamicModes": dynamic_modes,
        "suiteCount": len(suites.get("suites", {})),
        "suites": sorted(suites.get("suites", {}).keys()),
    },
    "envVarUsage": env_usage,
    "moduleSummary": {
        "apiRouteModules": len(list((root / "src" / "api").glob("*.rs"))),
        "dbRepositoryModules": len(list((root / "src" / "db").glob("*_repo.rs"))),
        "featureCfgSites": len(feature_cfg_sites),
    },
    "forbiddenImports": {
        "mediaImportsApi": len([line for line in media_api_imports.stdout.splitlines() if line]),
        "apiManualStageStarts": len([line for line in api_stage_starts.stdout.splitlines() if line]),
        "harnessStateFieldReads": len([line for line in harness_state_reads.stdout.splitlines() if line]),
    },
    "featureCfgSites": feature_cfg_sites,
}

pathlib.Path("target/source-audit.json").write_text(
    json.dumps(report, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY
echo "Wrote target/source-audit.json"

echo ""
if [ "$FAILED" -eq 1 ]; then
    echo "=== AUDIT FAILED ==="
    exit 1
else
    echo "=== AUDIT PASSED ==="
    exit 0
fi
