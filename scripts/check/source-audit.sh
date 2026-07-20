#!/usr/bin/env bash
# Source-wide audit automation to enforce clean layering, prevent growth of
# god files, and catch un-centralized environment variables.

set -euo pipefail

ROOT_DIR="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
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
SOURCE_LINE_LIMIT=999
SOURCE_LINE_WARNING=800
FRONTEND_SOURCE_LINE_LIMIT=2000
LARGE_FILE_REPORT=target/source-audit-large-files.jsonl
: > "$LARGE_FILE_REPORT"

SOURCE_ROOTS=()
for root in src web/ts test tests benches; do
    if [ -d "$root" ]; then
        SOURCE_ROOTS+=("$root")
    fi
done

find_audited_source_files() {
    if [ -f build.rs ]; then
        printf 'build.rs\0'
    fi
    find "${SOURCE_ROOTS[@]}" \
        \( -path '*/.local/artifacts' -o -path '*/.local/artifacts/*' \
            -o -path 'test/fixtures' -o -path 'test/fixtures/*' \) -prune \
        -o -type f \
        \( -name '*.rs' -o -name '*.ts' -o -name '*.mjs' -o -name '*.js' \) \
        -print0
}

classify_source_file() {
    local file="$1"
    case "$file" in
        build.rs)
            printf 'build-script'
            ;;
        benches/*.rs)
            printf 'benchmark'
            ;;
        tests/*.rs)
            printf 'integration-test'
            ;;
        src/bin/test_harness.rs|src/bin/test_harness/*.rs|test/harness/*.rs)
            printf 'harness'
            ;;
        src/*/tests/*.rs|src/*_test/*.rs|src/*_tests/*.rs|src/*/test.rs|src/*/tests.rs|src/*_test.rs|src/*_tests.rs)
            printf 'dedicated-test'
            ;;
        src/*.rs)
            printf 'production'
            ;;
        test/*.rs)
            printf 'harness'
            ;;
        web/ts/*|test/*)
            printf 'frontend-test-or-source'
            ;;
        *)
            printf 'other'
            ;;
    esac
}

declare -A RUST_CLASS_COUNTS=(
    [build-script]=0
    [production]=0
    [dedicated-test]=0
    [harness]=0
    [benchmark]=0
    [integration-test]=0
)
SOURCE_SIZE_FAILED=0
SOURCE_SIZE_WARNINGS=0
while IFS= read -r -d '' file; do
    lines=$(awk 'END { print NR }' "$file")
    classification=$(classify_source_file "$file")
    status=pass
    if [ "${file##*.}" = rs ]; then
        file_limit=$SOURCE_LINE_LIMIT
        warning_threshold=$SOURCE_LINE_WARNING
        language=rust
        RUST_CLASS_COUNTS["$classification"]=$((RUST_CLASS_COUNTS["$classification"] + 1))
        if [ "$lines" -gt "$file_limit" ]; then
            status=fail
            echo "FAIL [$classification]: $file has $lines raw lines (Rust hard maximum: $file_limit; 1000 fails)" >&2
            FAILED=1
            SOURCE_SIZE_FAILED=1
        elif [ "$lines" -ge "$warning_threshold" ]; then
            status=warn
            SOURCE_SIZE_WARNINGS=$((SOURCE_SIZE_WARNINGS + 1))
            echo "WARN [$classification]: $file has $lines raw lines (Rust pressure band: ${warning_threshold}-${file_limit})" >&2
        fi
    else
        file_limit=$FRONTEND_SOURCE_LINE_LIMIT
        warning_threshold=null
        language=typescript-or-javascript
        if [ "$lines" -gt "$file_limit" ]; then
            status=fail
            echo "FAIL [$classification]: $file has $lines raw lines (frontend hard maximum: $file_limit; 2001 fails)" >&2
            FAILED=1
            SOURCE_SIZE_FAILED=1
        fi
    fi
    printf '{"file":"%s","lines":%s,"limit":%s,"warningThreshold":%s,"language":"%s","classification":"%s","status":"%s"}\n' \
        "$file" "$lines" "$file_limit" "$warning_threshold" "$language" \
        "$classification" "$status" >> "$LARGE_FILE_REPORT"
done < <(find_audited_source_files)

echo "Line policies: Rust hard maximum ${SOURCE_LINE_LIMIT} (warn at ${SOURCE_LINE_WARNING}); TypeScript/JavaScript hard maximum ${FRONTEND_SOURCE_LINE_LIMIT}."
echo "Audited Rust files by responsibility:"
for classification in build-script production dedicated-test harness benchmark integration-test; do
    printf '  %-16s %s\n' "$classification:" "${RUST_CLASS_COUNTS[$classification]}"
done

if [ "$SOURCE_SIZE_FAILED" -eq 0 ]; then
    echo "OK: All audited files are within their language-specific raw-line maximum."
fi
if [ "$SOURCE_SIZE_WARNINGS" -gt 0 ]; then
    echo "WARN: ${SOURCE_SIZE_WARNINGS} Rust file(s) are in the ${SOURCE_LINE_WARNING}-${SOURCE_LINE_LIMIT} pressure band."
    echo "      Near-cap clustering is architectural pressure, not success; split by ownership before adding more code."
fi

# 3. Check for raw std::env::var usage outside src/config.rs and tests
echo ""
echo "Checking for inline std::env::var usage outside src/config.rs..."
# Exclude config.rs, main.rs (tokio runtime limits), test harness,
# tests, benches, test_fixtures.rs, lib.rs (config-chain helpers),
# ffmpeg_extract.rs (the documented FFMPEG_BIN_PATH fallback), planner
# (BackendPolicy::from_env), and restream-mcp (separate binary).
RAW_ENV_VARS=$(grep -rn "std::env::var" src/ \
    | grep -v "src/config.rs" \
    | grep -v "src/main.rs" \
    | grep -v "src/lib.rs" \
    | grep -v "src/ffmpeg_extract.rs" \
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

python3 - "$SOURCE_LINE_LIMIT" "$SOURCE_LINE_WARNING" "$FRONTEND_SOURCE_LINE_LIMIT" <<'PY'
import json
import pathlib
import re
import subprocess
import sys
import tomllib
from collections import Counter

line_limit = int(sys.argv[1])
warning_threshold = int(sys.argv[2])
frontend_line_limit = int(sys.argv[3])
root = pathlib.Path(".")

def rel(path: pathlib.Path) -> str:
    return path.as_posix()

def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")

source_files = ([root / "build.rs"] if (root / "build.rs").is_file() else []) + [
    path
    for base in [
        root / "src",
        root / "web" / "ts",
        root / "test",
        root / "tests",
        root / "benches",
    ]
    if base.exists()
    for path in base.rglob("*")
    if path.is_file()
    and path.suffix in {".rs", ".ts", ".mjs", ".js"}
    and ".local/artifacts/" not in rel(path)
    and not rel(path).startswith("test/fixtures/")
]

def classify(path: pathlib.Path) -> str:
    name = rel(path)
    parts = path.parts
    if path.suffix != ".rs":
        return "frontend-test-or-source"
    if name == "build.rs":
        return "build-script"
    if parts[0] == "benches":
        return "benchmark"
    if parts[0] == "tests":
        return "integration-test"
    if (
        name == "src/bin/test_harness.rs"
        or name.startswith("src/bin/test_harness/")
        or name.startswith("test/harness/")
        or parts[0] == "test"
    ):
        return "harness"
    if parts[0] == "src" and (
        path.name in {"test.rs", "tests.rs"}
        or path.stem.endswith("_test")
        or path.stem.endswith("_tests")
        or any(part.endswith("_test") or part.endswith("_tests") for part in parts[1:-1])
        or "tests" in parts[1:-1]
    ):
        return "dedicated-test"
    if parts[0] == "src":
        return "production"
    return "other"

line_counts = []
for path in source_files:
    lines = len(read(path).splitlines())
    is_rust = path.suffix == ".rs"
    file_limit = line_limit if is_rust else frontend_line_limit
    file_warning = warning_threshold if is_rust else None
    status = (
        "fail"
        if lines > file_limit
        else "warn"
        if file_warning is not None and lines >= file_warning
        else "pass"
    )
    line_counts.append(
        {
            "file": rel(path),
            "lines": lines,
            "limit": file_limit,
            "warningThreshold": file_warning,
            "language": "rust" if is_rust else "typescript-or-javascript",
            "classification": classify(path),
            "status": status,
        }
    )
line_counts.sort(key=lambda row: (-row["lines"], row["file"]))
rust_classifications = Counter(
    row["classification"]
    for row in line_counts
    if pathlib.Path(row["file"]).suffix == ".rs"
)

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

layer_rules = {
    "domain": {
        "forbiddenCrateRoots": {
            "agent_backends",
            "agent_core",
            "agent_mcp",
            "agent_plane",
            "api",
            "api_runtime_views",
            "application",
            "db",
            "infrastructure",
            "media",
            "planner",
            "runtime",
        },
        "forbiddenExternalRoots": set(),
    },
    "runtime": {
        "forbiddenCrateRoots": {
            "agent_backends",
            "agent_core",
            "agent_mcp",
            "agent_plane",
            "api",
            "api_runtime_views",
            "application",
            "db",
            "infrastructure",
            "media",
            "planner",
        },
        "forbiddenExternalRoots": set(),
    },
    "planner": {
        "forbiddenCrateRoots": {
            "agent_backends",
            "agent_core",
            "agent_mcp",
            "agent_plane",
            "api",
            "api_runtime_views",
            "application",
            "db",
            "infrastructure",
            "media",
        },
        "forbiddenExternalRoots": set(),
    },
    "agent_core": {
        "forbiddenCrateRoots": {
            "agent_backends",
            "agent_mcp",
            "agent_plane",
            "api",
            "api_runtime_views",
            "application",
            "db",
            "infrastructure",
            "media",
        },
        "forbiddenExternalRoots": {"reqwest"},
    },
    "db": {
        "forbiddenCrateRoots": {
            "agent_backends",
            "agent_core",
            "agent_mcp",
            "agent_plane",
            "api",
            "api_runtime_views",
            "application",
            "media",
            "planner",
        },
        "forbiddenExternalRoots": set(),
    },
    "application": {
        "forbiddenCrateRoots": {"api", "api_runtime_views"},
        "forbiddenExternalRoots": {"axum"},
    },
    "media": {
        "forbiddenCrateRoots": {
            "api",
            "api_runtime_views",
            "application",
            "db",
            "infrastructure",
        },
        "forbiddenExternalRoots": set(),
    },
}
crate_use_re = re.compile(
    r"(?m)^[ \t]*(?P<public>pub(?:\([^)]*\))?[ \t]+)?"
    r"use[ \t]+crate::(?P<tree>.*?);",
    re.DOTALL,
)
crate_reference_re = re.compile(
    r"\bcrate::(?P<dependency>[A-Za-z_][A-Za-z0-9_]*)(?:::|\b)"
)
external_reference_re = re.compile(
    r"\b(?P<dependency>[A-Za-z_][A-Za-z0-9_]*)::"
)

def mask_rust_comments(source: str) -> str:
    """Mask comments while preserving offsets and line numbers."""
    masked = list(source)
    index = 0
    block_depth = 0
    while index < len(source):
        if block_depth:
            if source.startswith("/*", index):
                masked[index:index + 2] = "  "
                block_depth += 1
                index += 2
            elif source.startswith("*/", index):
                masked[index:index + 2] = "  "
                block_depth -= 1
                index += 2
            else:
                if source[index] != "\n":
                    masked[index] = " "
                index += 1
        elif source.startswith("//", index):
            while index < len(source) and source[index] != "\n":
                masked[index] = " "
                index += 1
        elif source.startswith("/*", index):
            masked[index:index + 2] = "  "
            block_depth = 1
            index += 2
        else:
            index += 1
    return "".join(masked)

def split_top_level_use_items(tree: str):
    """Return (item, offset) pairs from one brace-grouped Rust use tree."""
    items = []
    item_start = 0
    depth = 0
    for index, character in enumerate(tree):
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
        elif character == "," and depth == 0:
            items.append((tree[item_start:index], item_start))
            item_start = index + 1
    items.append((tree[item_start:], item_start))
    return items

def crate_use_dependencies(source: str):
    """Find top-level crate owners in direct and grouped Rust use statements."""
    masked = mask_rust_comments(source)
    rows = []
    for match in crate_use_re.finditer(masked):
        tree = match.group("tree")
        stripped = tree.lstrip()
        leading = len(tree) - len(stripped)
        if stripped.startswith("{"):
            closing = stripped.rfind("}")
            if closing < 0:
                continue
            grouped = stripped[1:closing]
            grouped_offset = leading + 1
            items = split_top_level_use_items(grouped)
        else:
            grouped_offset = leading
            items = [(stripped, 0)]
        for item, item_offset in items:
            dependency_match = re.match(
                r"\s*(?P<dependency>[A-Za-z_][A-Za-z0-9_]*)\b",
                item,
            )
            if not dependency_match:
                continue
            dependency = dependency_match.group("dependency")
            if dependency in {"crate", "self", "super"}:
                continue
            dependency_offset = (
                match.start("tree")
                + grouped_offset
                + item_offset
                + dependency_match.start("dependency")
            )
            rows.append(
                {
                    "dependency": dependency,
                    "line": source.count("\n", 0, dependency_offset) + 1,
                    "public": bool(match.group("public")),
                    "statementStart": match.start(),
                    "statementEnd": match.end(),
                    "text": " ".join(source[match.start():match.end()].split()),
                }
            )
    return rows

dependency_parser_fixture = """\
pub use crate::{
    api::{ApiError, ApiResult},
    runtime::RuntimeState,
    self,
};
use crate::domain::Pipeline;
use crate::{media::Packet, planner::{Plan, Step}};
"""
dependency_parser_actual = [
    (row["dependency"], row["line"], row["public"])
    for row in crate_use_dependencies(dependency_parser_fixture)
]
dependency_parser_expected = [
    ("api", 2, True),
    ("runtime", 3, True),
    ("domain", 6, False),
    ("media", 7, False),
    ("planner", 7, False),
]
if dependency_parser_actual != dependency_parser_expected:
    raise RuntimeError(
        "crate use dependency parser self-test failed: "
        f"expected {dependency_parser_expected}, got {dependency_parser_actual}"
    )

def is_reexport_only_facade(source: str) -> bool:
    masked = mask_rust_comments(source)
    without_uses = crate_use_re.sub("", masked)
    without_attributes = re.sub(
        r"(?m)^[ \t]*#!?\[[^\n]*\][ \t]*$", "", without_uses
    )
    return not without_attributes.strip()

compatibility_marker_re = re.compile(
    r"\b(?:compatibility|deprecated|historical|migration)\b",
    re.IGNORECASE,
)

wrong_direction_imports = []
upward_compatibility_reexports = []
for layer, rule in layer_rules.items():
    layer_root = root / "src" / layer
    if not layer_root.exists():
        continue
    for path in sorted(layer_root.rglob("*.rs")):
        if classify(path) != "production":
            continue
        source = read(path)
        source_lines = source.splitlines()
        masked_source = mask_rust_comments(source)
        use_dependencies = crate_use_dependencies(source)
        facade_only = is_reexport_only_facade(source)
        recorded_wrong_directions = set()
        for use_dependency in use_dependencies:
            dependency_name = use_dependency["dependency"]
            dependency = f"crate::{dependency_name}"
            if dependency_name in rule["forbiddenCrateRoots"]:
                key = (use_dependency["line"], dependency)
                if key not in recorded_wrong_directions:
                    wrong_direction_imports.append(
                        {
                            "file": rel(path),
                            "line": use_dependency["line"],
                            "layer": layer,
                            "dependency": dependency,
                            "text": use_dependency["text"],
                            "action": (
                                f"move the dependency behind a lower-layer contract or an "
                                f"adapter owned above {layer}"
                            ),
                        }
                    )
                    recorded_wrong_directions.add(key)
            if not use_dependency["public"]:
                continue
            context_lines = source_lines[
                max(0, use_dependency["line"] - 7):use_dependency["line"] - 1
            ]
            has_compatibility_marker = bool(
                compatibility_marker_re.search("\n".join(context_lines))
            )
            same_owner = dependency_name == layer
            if same_owner and not (facade_only or has_compatibility_marker):
                continue
            if dependency_name in rule["forbiddenCrateRoots"]:
                relation = "wrong-direction"
                reason = "forbidden dependency is publicly re-exported"
                action = (
                    "remove the upward compatibility re-export after callers "
                    "import the owning lower-layer module"
                )
            elif same_owner:
                relation = "same-owner"
                reason = (
                    "re-export-only facade"
                    if facade_only
                    else "explicit compatibility or migration marker"
                )
                action = (
                    "confirm this same-owner facade is time-bounded; curated "
                    "owner APIs with real implementation may remain"
                )
            else:
                relation = "allowed-cross-owner"
                reason = "allowed or lateral owner is publicly re-exported"
                action = (
                    "confirm this cross-owner facade is a deliberate stable API "
                    "or name the migration condition for removing it"
                )
            upward_compatibility_reexports.append(
                {
                    "file": rel(path),
                    "line": use_dependency["line"],
                    "layer": layer,
                    "dependency": dependency,
                    "text": use_dependency["text"],
                    "relation": relation,
                    "reason": reason,
                    "action": action,
                }
            )
        for line_no, line in enumerate(masked_source.splitlines(), start=1):
            crate_dependencies = {
                match.group("dependency")
                for match in crate_reference_re.finditer(line)
                if match.group("dependency") in rule["forbiddenCrateRoots"]
            }
            external_dependencies = {
                match.group("dependency")
                for match in external_reference_re.finditer(line)
                if match.group("dependency") in rule["forbiddenExternalRoots"]
            }
            for dependency in [
                *(f"crate::{name}" for name in sorted(crate_dependencies)),
                *sorted(external_dependencies),
            ]:
                key = (line_no, dependency)
                if key in recorded_wrong_directions:
                    continue
                row = {
                    "file": rel(path),
                    "line": line_no,
                    "layer": layer,
                    "dependency": dependency,
                    "text": source_lines[line_no - 1].strip(),
                    "action": (
                        f"move the dependency behind a lower-layer contract or an "
                        f"adapter owned above {layer}"
                    ),
                }
                wrong_direction_imports.append(row)
                recorded_wrong_directions.add(key)

definition_re = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+"
    r"(?P<name>[A-Z][A-Za-z0-9_]*)\b"
)
inherent_impl_re = re.compile(
    r"^\s*impl(?:\s*<[^>{}]*>)?\s+(?P<name>[A-Z][A-Za-z0-9_]*)"
    r"(?:\s*<[^>{}]*>)?\s*(?:where[^{]+)?\{"
)
production_rust_files = [
    path
    for path in sorted((root / "src").rglob("*.rs"))
    if classify(path) == "production"
]
type_owners = {}
for path in production_rust_files:
    for line in read(path).splitlines():
        match = definition_re.match(line)
        if match:
            type_owners.setdefault(match.group("name"), set()).add(rel(path))

external_inherent_impls = []
for path in production_rust_files:
    for line_no, line in enumerate(read(path).splitlines(), start=1):
        match = inherent_impl_re.match(line)
        if not match:
            continue
        owners = type_owners.get(match.group("name"), set())
        if len(owners) != 1:
            continue
        owner = next(iter(owners))
        if owner == rel(path):
            continue
        owner_layer = pathlib.Path(owner).parts[1]
        impl_layer = path.parts[1]
        external_inherent_impls.append(
            {
                "type": match.group("name"),
                "ownerFile": owner,
                "implFile": rel(path),
                "line": line_no,
                "ownerLayer": owner_layer,
                "implLayer": impl_layer,
                "crossLayer": owner_layer != impl_layer,
                "action": (
                    "prefer an adapter trait or owner-local constructor"
                    if owner_layer != impl_layer
                    else "confirm the extension impl is an intentional ownership seam"
                ),
            }
        )

cargo_manifest = tomllib.loads(read(root / "Cargo.toml"))
features = {
    name: list(enabled)
    for name, enabled in cargo_manifest.get("features", {}).items()
}
feature_edges = [
    {
        "feature": feature,
        "enables": enabled,
        "kind": "feature" if enabled in features else "dependency",
    }
    for feature, enabled_features in sorted(features.items())
    for enabled in enabled_features
]

def feature_closure(feature: str) -> set[str]:
    enabled = set()
    pending = [feature]
    while pending:
        current = pending.pop()
        if current in enabled:
            continue
        enabled.add(current)
        for dependency in features.get(current, []):
            if dependency in features:
                pending.append(dependency)
    return enabled

module_gate_re = re.compile(r'^\s*#\[cfg\(feature\s*=\s*"([^"]+)"\)\]\s*$')
module_re = re.compile(r"^\s*pub\s+mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")

def module_feature_gates(path: pathlib.Path):
    rows = []
    pending_gate = None
    for line_no, line in enumerate(read(path).splitlines(), start=1):
        gate_match = module_gate_re.match(line)
        if gate_match:
            pending_gate = gate_match.group(1)
            continue
        module_match = module_re.match(line)
        if module_match:
            rows.append(
                {
                    "file": rel(path),
                    "line": line_no,
                    "module": module_match.group(1),
                    "feature": pending_gate,
                }
            )
        if line.strip() and not line.lstrip().startswith("#["):
            pending_gate = None
    return rows

module_gates = [
    *module_feature_gates(root / "src" / "lib.rs"),
    *module_feature_gates(root / "src" / "agent_backends" / "mod.rs"),
]
module_gate_lookup = {
    (row["file"], row["module"]): row["feature"]
    for row in module_gates
}

def static_feature_check(name: str, passed: bool, detail: str):
    return {"name": name, "passed": passed, "detail": detail}

audited_feature_names = [
    "mcp-core",
    "mcp-server",
    "mcp-http-backend",
    "mcp-embedded",
]
audited_feature_closures = {
    feature: feature_closure(feature)
    for feature in audited_feature_names
}
mcp_core_closure = audited_feature_closures["mcp-core"]
mcp_server_closure = audited_feature_closures["mcp-server"]
feature_static_checks = [
    static_feature_check(
        "mcp-core-does-not-enable-agent-plane",
        "agent-plane" not in mcp_core_closure,
        "the lower MCP contract/handler feature must compile without the higher agent plane",
    ),
    static_feature_check(
        "mcp-server-does-not-enable-agent-plane",
        "agent-plane" not in mcp_server_closure,
        "the MCP server transport must not hide an agent-plane dependency",
    ),
    static_feature_check(
        "agent-plane-module-is-locally-gated",
        module_gate_lookup.get(("src/lib.rs", "agent_plane")) == "agent-plane",
        "src/lib.rs::agent_plane must remain gated by agent-plane",
    ),
    static_feature_check(
        "mcp-module-is-locally-gated",
        module_gate_lookup.get(("src/lib.rs", "agent_mcp")) == "mcp-core",
        "src/lib.rs::agent_mcp must remain gated by mcp-core",
    ),
    static_feature_check(
        "http-adapter-is-locally-gated",
        module_gate_lookup.get(("src/agent_backends/mod.rs", "http"))
        == "mcp-http-backend",
        "the HTTP adapter must carry its own mcp-http-backend cfg gate",
    ),
    static_feature_check(
        "embedded-adapter-is-locally-gated",
        module_gate_lookup.get(("src/agent_backends/mod.rs", "in_process"))
        == "mcp-embedded",
        "the in-process adapter must carry its own mcp-embedded cfg gate",
    ),
]
negative_feature_matrix = [
    {
        "purpose": "prove the lower MCP surface compiles without agent-plane",
        "requestedFeatures": ["mcp-core"],
        "command": (
            "scripts/build/resource-limit.sh cargo check --lib "
            "--no-default-features --features mcp-core"
        ),
        "mustNotEnable": ["agent-plane", "agent-execution", "mcp-embedded"],
    },
    {
        "purpose": "prove the MCP transport compiles without agent-plane",
        "requestedFeatures": ["mcp-server"],
        "command": (
            "scripts/build/resource-limit.sh cargo check --lib "
            "--no-default-features --features mcp-server"
        ),
        "mustNotEnable": ["agent-plane", "agent-execution", "mcp-embedded"],
    },
    {
        "purpose": "prove the standalone HTTP-backed sidecar feature set",
        "requestedFeatures": ["mcp-server", "mcp-http-backend"],
        "command": (
            "scripts/build/resource-limit.sh cargo check --bin restream-mcp "
            "--no-default-features --features mcp-server,mcp-http-backend"
        ),
        "mustNotEnable": ["agent-plane", "agent-execution", "mcp-embedded"],
    },
    {
        "purpose": "prove the explicitly integrated adapter feature set",
        "requestedFeatures": ["mcp-embedded"],
        "command": (
            "scripts/build/resource-limit.sh cargo check --lib "
            "--no-default-features --features mcp-embedded"
        ),
        "mustEnable": ["mcp-core", "agent-plane"],
    },
]
for row in negative_feature_matrix:
    closure = set()
    for feature in row["requestedFeatures"]:
        closure.update(feature_closure(feature))
    checks = [
        {
            "claim": "mustEnable",
            "feature": feature,
            "passed": feature in closure,
        }
        for feature in row.get("mustEnable", [])
    ]
    checks.extend(
        {
            "claim": "mustNotEnable",
            "feature": feature,
            "passed": feature not in closure,
        }
        for feature in row.get("mustNotEnable", [])
    )
    row["closure"] = sorted(closure)
    row["checks"] = checks
    row["topologyPassed"] = all(check["passed"] for check in checks)
    row["executed"] = False
    row["exitCode"] = None
    row["compilePassed"] = None

failed_negative_feature_claims = [
    {
        "purpose": row["purpose"],
        **check,
    }
    for row in negative_feature_matrix
    for check in row["checks"]
    if not check["passed"]
]

def snake_to_camel(value: str) -> str:
    head, *tail = value.split("_")
    return head + "".join(part[:1].upper() + part[1:] for part in tail)

api_view_models = read(root / "src" / "api_view_models.rs")
egress_match = re.search(
    r"pub\(crate\) fn egress_runtime_json\([\s\S]*?pub\(crate\) fn output_runtime_explanation_json",
    api_view_models,
)
api_output_status_fields = set()
if egress_match:
    body = egress_match.group(0)
    api_output_status_fields.update(re.findall(r'"([A-Za-z][A-Za-z0-9]*)"\s*:', body))
    api_output_status_fields.update(re.findall(r'value\["([A-Za-z][A-Za-z0-9]*)"\]', body))

harness_api_client = read(root / "src" / "bin" / "test_harness" / "api_client.rs")
harness_status_match = re.search(
    r"struct ApiOutputStatus \{(?P<body>[\s\S]*?)\n\}",
    harness_api_client,
)
harness_output_status_fields = set()
if harness_status_match:
    harness_output_status_fields.update(
        snake_to_camel(name)
        for name in re.findall(
            r"pub\(crate\)\s+([A-Za-z_][A-Za-z0-9_]*)\s*:",
            harness_status_match.group("body"),
        )
    )

output_status_missing_in_harness = sorted(
    api_output_status_fields - harness_output_status_fields
)
output_status_extra_in_harness = sorted(
    harness_output_status_fields - api_output_status_fields
)

report = {
    "sourceLineLimit": line_limit,
    "sourceLineLimitScope": "rust",
    "sourceLineWarning": warning_threshold,
    "sourceLineLimits": {
        "rust": line_limit,
        "typescriptOrJavaScript": frontend_line_limit,
    },
    "sourceLinePolicy": {
        "unit": "raw-lines",
        "languages": {
            "rust": {
                "hardMaximum": line_limit,
                "firstFailingLineCount": line_limit + 1,
                "warningRange": {
                    "minimum": warning_threshold,
                    "maximum": line_limit,
                },
            },
            "typescriptOrJavaScript": {
                "hardMaximum": frontend_line_limit,
                "firstFailingLineCount": frontend_line_limit + 1,
                "warningRange": None,
            },
        },
        "interpretation": (
            "Near-cap clustering is architectural pressure, not success; "
            "split by ownership before adding more code."
        ),
    },
    "largestFiles": line_counts[:20],
    "lineCounts": line_counts,
    "rustClassificationSummary": {
        classification: rust_classifications.get(classification, 0)
        for classification in [
            "build-script",
            "production",
            "dedicated-test",
            "harness",
            "benchmark",
            "integration-test",
        ]
    },
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
    "outputStatusSchema": {
        "source": "src/api_view_models.rs::egress_runtime_json",
        "harnessDto": "src/bin/test_harness/api_client.rs::ApiOutputStatus",
        "apiFields": sorted(api_output_status_fields),
        "harnessFields": sorted(harness_output_status_fields),
        "missingInHarness": output_status_missing_in_harness,
        "extraInHarness": output_status_extra_in_harness,
    },
    "featureCfgSites": feature_cfg_sites,
    "boundaryHazards": {
        "rules": [
            {
                "layer": layer,
                "forbiddenCrateRoots": sorted(rule["forbiddenCrateRoots"]),
                "forbiddenExternalRoots": sorted(rule["forbiddenExternalRoots"]),
            }
            for layer, rule in sorted(layer_rules.items())
        ],
        "wrongDirectionImports": wrong_direction_imports,
        "upwardCompatibilityReexports": upward_compatibility_reexports,
        "externalInherentImpls": external_inherent_impls,
        "blockingCount": len(wrong_direction_imports),
        "reviewCount": (
            len(upward_compatibility_reexports) + len(external_inherent_impls)
        ),
        "parserChecks": {
            "groupedMultilineCrateUse": {
                "passed": True,
                "expected": dependency_parser_expected,
                "actual": dependency_parser_actual,
            },
        },
    },
    "featureTopology": {
        "features": features,
        "edges": feature_edges,
        "closures": {
            feature: sorted(closure)
            for feature, closure in audited_feature_closures.items()
        },
        "moduleGates": module_gates,
        "staticChecks": feature_static_checks,
        "negativeMatrix": negative_feature_matrix,
        "historicalRegression": {
            "commit": "72f9441e",
            "featureEdge": "mcp-core -> agent-plane",
            "lesson": (
                "compile the lower feature without the higher feature and "
                "keep adapter cfg gates beside the adapter module"
            ),
        },
    },
}

pathlib.Path("target/source-audit.json").write_text(
    json.dumps(report, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

if output_status_missing_in_harness:
    print(
        "FAIL: Harness ApiOutputStatus is missing API output status fields: "
        + ", ".join(output_status_missing_in_harness),
        file=sys.stderr,
    )
blocking_boundary_hazards = len(wrong_direction_imports)
failed_feature_checks = [
    check for check in feature_static_checks if not check["passed"]
]
print("")
print("Checking candidate boundary hazards...")
if blocking_boundary_hazards:
    for hazard in wrong_direction_imports:
        print(
            f"FAIL: {hazard['file']}:{hazard['line']} "
            f"{hazard['layer']} imports {hazard['dependency']}: "
            f"{hazard['action']}",
            file=sys.stderr,
        )
else:
    print("OK: No encoded wrong-direction imports.")
print(
    "REVIEW: "
    f"{len(upward_compatibility_reexports)} compatibility/facade re-export "
    "site(s); inspect "
    "boundaryHazards.upwardCompatibilityReexports in target/source-audit.json."
)
cross_layer_impls = sum(
    1 for row in external_inherent_impls if row["crossLayer"]
)
print(
    "REVIEW: "
    f"{len(external_inherent_impls)} external inherent impl site(s) "
    f"({cross_layer_impls} cross-layer); inspect "
    "boundaryHazards.externalInherentImpls in target/source-audit.json."
)
for check in failed_feature_checks:
    print(f"FAIL: {check['name']}: {check['detail']}", file=sys.stderr)
if not failed_feature_checks:
    print("OK: Feature topology keeps lower MCP features independent of agent-plane.")
for failure in failed_negative_feature_claims:
    print(
        f"FAIL: {failure['purpose']}: {failure['claim']} "
        f"{failure['feature']}",
        file=sys.stderr,
    )
if not failed_negative_feature_claims:
    evaluated_claim_count = sum(
        len(row["checks"]) for row in negative_feature_matrix
    )
    print(
        "OK: "
        f"All {evaluated_claim_count} negative feature-matrix topology claims pass."
    )
print("Negative feature matrix (run after feature-boundary changes):")
for row in negative_feature_matrix:
    print(
        f"  [TOPOLOGY {('PASS' if row['topologyPassed'] else 'FAIL')}; "
        "COMPILE NOT RUN] "
        f"{row['command']}"
    )

if (
    output_status_missing_in_harness
    or blocking_boundary_hazards
    or failed_feature_checks
    or failed_negative_feature_claims
):
    sys.exit(1)
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
