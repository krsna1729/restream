#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/check/staged-gate-router.sh [options]

Route changed files to the narrowest repo checks from AGENTS.md.
Run only fast pre-commit gates automatically; print heavier follow-up gates.

Options:
  --staged          inspect staged changes (default)
  --unstaged        inspect unstaged working-tree changes
  --base <ref>      inspect changes from merge-base(<ref>, HEAD) to HEAD
  --dry-run         print selected gates without running them
  -h, --help        show this help
EOF
}

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
MODE="staged"
BASE_REF=""
DRY_RUN=0

while (($# > 0)); do
    case "$1" in
        --staged)
            MODE="staged"
            ;;
        --unstaged)
            MODE="unstaged"
            ;;
        --base)
            if (($# < 2)); then
                echo "staged-gate-router: --base requires a ref" >&2
                exit 2
            fi
            MODE="base"
            BASE_REF="$2"
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "staged-gate-router: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

cd "$ROOT"

case "$MODE" in
    staged)
        mapfile -t changed_files < <(git diff --cached --name-only --diff-filter=ACMR)
        ;;
    unstaged)
        mapfile -t changed_files < <(git diff --name-only --diff-filter=ACMR)
        ;;
    base)
        mapfile -t changed_files < <(git diff --name-only --diff-filter=ACMR "$BASE_REF"...HEAD)
        ;;
esac

if ((${#changed_files[@]} == 0)); then
    echo "staged-gate-router: no changed files for ${MODE} diff"
    exit 0
fi

rust_files=()
shell_files=()
frontend_files=()
module_filters=()
follow_up_gates=()
manual_recommendations=()
declare -A auto_gates=()
declare -A seen_follow_ups=()
declare -A seen_modules=()
declare -A seen_manual_recommendations=()

add_auto_gate() {
    auto_gates["$1"]=1
}

add_module_filter() {
    local filter="$1"
    [[ -n "$filter" ]] || return 0
    if [[ -z "${seen_modules[$filter]+x}" ]]; then
        seen_modules["$filter"]=1
        module_filters+=("$filter")
    fi
}

add_follow_up_gate() {
    local gate="$1"
    if [[ -z "${seen_follow_ups[$gate]+x}" ]]; then
        seen_follow_ups["$gate"]=1
        follow_up_gates+=("$gate")
    fi
}

add_manual_recommendation() {
    local gate="$1"
    if [[ -z "${seen_manual_recommendations[$gate]+x}" ]]; then
        seen_manual_recommendations["$gate"]=1
        manual_recommendations+=("$gate")
    fi
}

module_filter_for_path() {
    local file="$1"
    local stem

    case "$file" in
        src/bin/* | src/main.rs | src/lib.rs | */mod.rs)
            return 0
            ;;
        tests/*.rs)
            stem="${file##*/}"
            stem="${stem%.rs}"
            printf '%s\n' "$stem"
            ;;
        benches/*.rs)
            return 0
            ;;
        *)
            if [[ "$file" =~ ^src/.+\.rs$ ]]; then
                stem="${file##*/}"
                stem="${stem%.rs}"
                printf '%s\n' "$stem"
            fi
            ;;
    esac
}

is_lifecycle_file() {
    case "$1" in
        src/media/engine.rs | \
            src/media/srt.rs | \
            src/media/ts_chunk_ring.rs | \
            src/media/avio.rs | \
            src/media/recording.rs | src/media/recording/*.rs | \
            src/media/file_ingest.rs | \
            src/media/external_transcoder.rs | src/media/external_transcoder/*.rs)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_frontend_file() {
    [[ "$1" =~ ^web/ts/.*\.ts$ ]] ||
        [[ "$1" =~ ^web/pages/.*\.html$ ]] ||
        [[ "$1" == "web/styles/input.css" ]]
}

is_api_contract_file() {
    case "$1" in
        src/api.rs | src/api/*.rs | \
            src/api_runtime_views.rs | src/api_runtime_views/*.rs | \
            src/api_view_models.rs | \
            src/bin/test_harness/api_client.rs | \
            web/ts/core/api.ts | web/ts/types.ts | \
            docs/api-reference.md)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_fixture_or_harness_file() {
    case "$1" in
        test/* | tests/fixtures.rs | src/test_fixtures.rs | \
            benches/* | scripts/fixtures/* | scripts/harness/* | \
            scripts/build/bench-harness.sh | src/bin/test_harness.rs | src/bin/test_harness/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_hot_path_file() {
    case "$1" in
        src/media/* | benches/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_protocol_file() {
    case "$1" in
        src/media/rtmp.rs | src/media/rtmp/* | \
            src/media/srt.rs | src/media/srt_*.rs | src/media/srt/* | \
            src/media/hls.rs | src/media/hls/*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

diff_contains_concurrency_change() {
    ((${#rust_files[@]} > 0)) || return 1

    case "$MODE" in
        staged)
            git diff --cached -U0 -- "${rust_files[@]}"
            ;;
        unstaged)
            git diff -U0 -- "${rust_files[@]}"
            ;;
        base)
            git diff -U0 "$BASE_REF"...HEAD -- "${rust_files[@]}"
            ;;
    esac | rg -q '^\+.*\b(tokio::spawn|spawn_blocking|std::thread|thread::spawn|Mutex|RwLock|Atomic[A-Za-z]*|mpsc::|watch::|broadcast::|oneshot::|Notify|Semaphore|JoinHandle|catch_unwind)\b'
}

for file in "${changed_files[@]}"; do
    if [[ "$file" == *.md ]] || [[ "$file" == "scripts/check/docs.mjs" ]]; then
        add_auto_gate "node scripts/check/docs.mjs"
    fi

    if [[ "$file" == *.rs ]]; then
        rust_files+=("$file")
        add_auto_gate "cargo fmt --all --check"
    fi

    if [[ "$file" == *.sh ]] || [[ "$file" == .githooks/* ]]; then
        shell_files+=("$file")
        add_auto_gate "bash -n staged shell files"
    fi

    if is_frontend_file "$file"; then
        frontend_files+=("$file")
        add_auto_gate "npm run format:check"
        add_follow_up_gate "npm run test:frontend"
    fi

    if is_lifecycle_file "$file"; then
        add_follow_up_gate "scripts/check/concurrency/contract.sh"
    fi

    if is_api_contract_file "$file"; then
        add_follow_up_gate "scripts/check/api-contract.sh"
    fi

    if is_fixture_or_harness_file "$file"; then
        add_follow_up_gate "scripts/check/fixture-discipline.sh"
    fi

    if is_hot_path_file "$file"; then
        add_manual_recommendation "relevant scripts/build/resource-limit.sh cargo bench --bench <name>"
    fi

    if is_protocol_file "$file"; then
        add_manual_recommendation "scripts/build/resource-limit.sh target/debug/test_harness correctness*"
    fi
done

if diff_contains_concurrency_change; then
    add_follow_up_gate "scripts/check/concurrency/fast.sh"
fi

for file in "${rust_files[@]}"; do
    if is_lifecycle_file "$file" || is_fixture_or_harness_file "$file"; then
        continue
    fi
    filter="$(module_filter_for_path "$file")"
    add_module_filter "$filter"
done

if ((${#module_filters[@]} > 0)); then
    for filter in "${module_filters[@]}"; do
        add_follow_up_gate "scripts/build/resource-limit.sh cargo test ${filter}"
    done
fi

echo "staged-gate-router: ${MODE} diff selected ${#changed_files[@]} file(s)"
for file in "${changed_files[@]}"; do
    echo "  $file"
done

echo
echo "staged-gate-router: fast pre-commit gates"
if ((${#auto_gates[@]} == 0)); then
    echo "  (none)"
else
    for gate in "${!auto_gates[@]}"; do
        echo "  $gate"
    done | sort
fi

if ((${#follow_up_gates[@]} > 0)); then
    echo
    echo "staged-gate-router: recommended follow-up gates"
    for gate in "${follow_up_gates[@]}"; do
        echo "  $gate"
    done
fi

if ((${#manual_recommendations[@]} > 0)); then
    echo
    echo "staged-gate-router: additional manual recommendations"
    for gate in "${manual_recommendations[@]}"; do
        echo "  $gate"
    done
fi

if ((DRY_RUN)); then
    exit 0
fi

run_gate() {
    echo
    printf 'staged-gate-router: running'
    printf ' %q' "$@"
    printf '\n'
    if ! "$@"; then
        echo
        printf 'staged-gate-router: failed:'
        printf ' %q' "$@"
        printf '\n'
        exit 1
    fi
}

if [[ -n "${auto_gates["cargo fmt --all --check"]+x}" ]]; then
    run_gate cargo fmt --all --check
fi

if [[ -n "${auto_gates["bash -n staged shell files"]+x}" ]]; then
    for file in "${shell_files[@]}"; do
        run_gate bash -n "$file"
    done
fi

if [[ -n "${auto_gates["npm run format:check"]+x}" ]]; then
    run_gate npm run format:check
fi

if [[ -n "${auto_gates["node scripts/check/docs.mjs"]+x}" ]]; then
    run_gate node scripts/check/docs.mjs
fi
