#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v node >/dev/null 2>&1; then
  echo "Node.js is required but was not found in PATH."
  exit 1
fi

version="$(node -p 'process.versions.node')"
major="${version%%.*}"
rest="${version#*.}"
minor="${rest%%.*}"

if [[ "$major" -lt 20 ]] || [[ "$major" -eq 20 && "$minor" -lt 19 ]]; then
  echo "Unsupported Node.js version: $version"
  echo "This project requires Node.js >= 20.19.0 (recommended: 22.x)."
  if [[ -f .nvmrc ]]; then
    echo "Tip: run 'nvm use' (or install nvm) to switch to the version in .nvmrc."
  fi
  exit 1
fi

echo "Using Node.js $version"

STAMP_FILE=".deps-stamp"

needs_install=0
if [[ ! -d node_modules ]]; then
  needs_install=1
elif [[ ! -f "$STAMP_FILE" ]]; then
  needs_install=1
elif [[ package.json -nt "$STAMP_FILE" || package-lock.json -nt "$STAMP_FILE" ]]; then
  needs_install=1
fi

if [[ "${FORCE_NPM_CI:-0}" == "1" ]]; then
  needs_install=1
fi

if [[ "$needs_install" == "1" ]]; then
  echo "Installing dependencies (npm ci)..."
  npm ci
  touch "$STAMP_FILE"
else
  echo "Dependencies are up to date; skipping npm ci."
fi
