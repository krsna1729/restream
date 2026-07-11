#!/usr/bin/env bash
set -euo pipefail

repo_root="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
cd "$repo_root"

if [[ -z "${TMPDIR:-}" || ! -d "${TMPDIR:-}" || ! -w "${TMPDIR:-}" ]]; then
  export TMPDIR=/tmp
fi

scripts/build/resource-limit.sh cargo build --profile bench --bin restream --bin test_harness

# Cargo hardcodes target/release as the output dir for a profile named
# "bench" (dir-name cannot be overridden for built-in profile names, and the
# profile can't be renamed either since `cargo bench` always resolves it by
# name) — this copy is the only bridge from that fixed location into
# target/bench/, which the rest of the repo treats as the canonical home for
# bench-profile binaries. Do not duplicate this copy elsewhere; scripts that
# need a bench-profile binary should depend on this script instead.
mkdir -p target/bench
cp target/release/restream target/bench/restream
cp target/release/test_harness target/bench/test_harness

for binary in target/bench/restream target/bench/test_harness; do
  if [[ ! -x "$binary" ]]; then
    echo "expected bench-profile binary missing: $binary" >&2
    exit 1
  fi
done

cat <<'EOF'
Bench-profile measurement binaries are ready:
  target/bench/restream
  target/bench/test_harness

Use scripts/harness/run.sh for measurement modes so bench binaries stay
fresh and launches remain comparable.
EOF
