#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

usage() {
  cat <<'EOF' >&2
usage:
  scripts/run-bench-harness.sh [--prepare] <mode> [-- <harness args...>]
  scripts/run-bench-harness.sh --prepare
  scripts/run-bench-harness.sh --help

examples:
  scripts/run-bench-harness.sh mixed.matrix
  N_PER_GROUP=1 scripts/run-bench-harness.sh mixed.fast-breadth -- --no-netns
  BENCH_BUILD=if-needed scripts/run-bench-harness.sh resource-sweep
  scripts/run-bench-harness.sh --prepare

notes:
  - BENCH_BUILD controls build behavior: always|if-needed|never
  - default BENCH_BUILD=if-needed rebuilds when src/, Cargo.toml, build.rs,
    or rust-toolchain.toml is newer than target/bench/test_harness
EOF
}

needs_bench_rebuild() {
  local bin=$1
  if [[ ! -x "$bin" ]]; then
    return 0
  fi

  local stamp
  stamp=$(stat -c '%Y' "$bin")
  local newest
  newest=$(find src scripts test/harness Cargo.toml build.rs rust-toolchain.toml \
    -type f -not -path '*/target/*' -printf '%T@\n' 2>/dev/null | sort -nr | head -n1)
  if [[ -z "$newest" ]]; then
    return 1
  fi

  local newest_int=${newest%%.*}
  (( newest_int > stamp ))
}

prepare_only=0
if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
fi
if [[ ${1:-} == "--prepare" ]]; then
  prepare_only=1
  shift
fi

if [[ $prepare_only -eq 0 && $# -lt 1 ]]; then
  usage
  exit 2
fi

mode=${1:-}
if [[ $# -gt 0 ]]; then
  shift
fi

harness_args=()
if [[ ${1:-} == "--" ]]; then
  shift
  harness_args=("$@")
elif [[ $# -gt 0 ]]; then
  harness_args=("$@")
fi

bin=${HARNESS_BIN:-target/bench/test_harness}

build_mode=${BENCH_BUILD:-if-needed}
case "$build_mode" in
  1|true|TRUE|always)
    ./scripts/build-bench-harness.sh
    ;;
  0|false|FALSE|never)
    ;;
  if-needed)
    if needs_bench_rebuild "$bin"; then
      ./scripts/build-bench-harness.sh
    fi
    ;;
  *)
    echo "invalid BENCH_BUILD value '$build_mode'; expected always|if-needed|never" >&2
    exit 2
    ;;
esac

if [[ ! -x "$bin" ]]; then
  echo "missing harness binary at $bin; run scripts/build-bench-harness.sh" >&2
  exit 1
fi

if [[ $prepare_only -eq 1 ]]; then
  exit 0
fi

scripts/resource-limit "$bin" "$mode" "${harness_args[@]}"
