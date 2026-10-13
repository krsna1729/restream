#!/usr/bin/env bash
# Compiles web/ts to one plain-JavaScript module per source file in $1, for
# `node --test`.
set -euo pipefail
out="$1"
rm -rf "$out"
npx tsc -p tsconfig.frontend-node-test.json --outDir "$out"
