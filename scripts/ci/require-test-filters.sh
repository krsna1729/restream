#!/usr/bin/env bash
# Fail when any libtest name filter selects no test, so a renamed or moved
# module cannot silently drop out of a narrow CI job (libtest exits 0 on
# "0 passed; N filtered out").
#
# Usage: require-test-filters.sh <cargo test command ...> -- <filter>...
# Every argument after the first `--` is one filter; each is listed alone.
set -euo pipefail

command=()
while (($#)) && [[ $1 != -- ]]; do
  command+=("$1")
  shift
done
[[ ${1:-} == -- ]] || { echo "usage: $0 <cargo test ...> -- <filter>..." >&2; exit 2; }
shift
(($#)) || { echo "no filters given" >&2; exit 2; }

status=0
for filter in "$@"; do
  count=$("${command[@]}" -- "$filter" --list --format terse 2>/dev/null | grep -c ': test$' || true)
  if [[ $count -eq 0 ]]; then
    echo "filter '$filter' selects no test" >&2
    status=1
  else
    echo "filter '$filter': $count tests"
  fi
done
exit $status
