#!/usr/bin/env bash
# Release-profile Restream + test_harness for qualification and performance
# evidence (capacity ramps, A/B runs, profiles). Day-to-day iteration keeps
# scripts/build/bench-harness.sh (same optimisation flags, incremental).
# Output: target/qual-release/{restream,test_harness}, kept apart from
# target/release, which the bench profile also writes (a Cargo quirk).
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

if [[ -z "${TMPDIR:-}" || ! -d "${TMPDIR:-}" || ! -w "${TMPDIR:-}" ]]; then
  export TMPDIR=/tmp
fi

scripts/build/resource-limit.sh cargo build --release --bin restream --bin test_harness

mkdir -p target/qual-release
cp target/release/restream target/qual-release/restream
cp target/release/test_harness target/qual-release/test_harness
git rev-parse HEAD > target/qual-release/COMMIT 2>/dev/null || true
if [[ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]]; then
  echo dirty >> target/qual-release/COMMIT
fi
echo "release binaries: target/qual-release (commit $(head -1 target/qual-release/COMMIT))"
