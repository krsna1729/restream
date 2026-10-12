#!/usr/bin/env bash
# Compiles web/ts to one plain-JavaScript module per source file in $1, for
# `node --test`; the React entries are replaced by node stubs.
set -euo pipefail
out="$1"
rm -rf "$out"
npx tsc -p tsconfig.frontend-node-test.json --outDir "$out"
cp test/support/frontend-v2-node-stubs/app/*.js "$out/app/"
