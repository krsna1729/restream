#!/usr/bin/env bash
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
CHECKER="$ROOT/scripts/agent/worktree.sh"
TMP_BASE="${TMPDIR:-/tmp}"
WORK_DIR="$(mktemp -d "$TMP_BASE/restream-worktree-frontend-deps.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

write_root_manifest() {
    local root="$1"

    mkdir -p "$root"
    cat >"$root/package.json" <<'JSON'
{
  "private": true,
  "dependencies": {
    "hls.js": "*",
    "react": "*",
    "react-dom": "*"
  },
  "devDependencies": {
    "@tailwindcss/cli": "*",
    "@types/react": "*",
    "@types/react-dom": "*",
    "tailwindcss": "*",
    "typescript": "*",
    "vite": "*"
  }
}
JSON
}

write_package() {
    local root="$1"
    local name="$2"
    local package_root="$root/node_modules/$name"

    mkdir -p "$package_root"
    printf '{"name":"%s","version":"1.0.0"}\n' "$name" >"$package_root/package.json"
}

write_legacy_subset() {
    local root="$1"

    mkdir -p "$root/node_modules/.bin" "$root/node_modules/hls.js/dist"
    install -m 755 /dev/null "$root/node_modules/.bin/tsc"
    install -m 755 /dev/null "$root/node_modules/.bin/tailwindcss"
    touch "$root/node_modules/hls.js/dist/hls.min.js"
}

write_complete_fixture() {
    local root="$1"
    local package

    write_legacy_subset "$root"
    install -m 755 /dev/null "$root/node_modules/.bin/vite"
    for package in \
        hls.js \
        react \
        react-dom \
        @tailwindcss/cli \
        @types/react \
        @types/react-dom \
        tailwindcss \
        typescript \
        vite; do
        write_package "$root" "$package"
    done
    touch \
        "$root/node_modules/react/jsx-runtime.js" \
        "$root/node_modules/react-dom/client.js" \
        "$root/node_modules/@types/react/index.d.ts" \
        "$root/node_modules/@types/react-dom/index.d.ts"
}

stale_root="$WORK_DIR/stale"
write_root_manifest "$stale_root"
write_legacy_subset "$stale_root"
if "$CHECKER" --check-frontend-deps "$stale_root"; then
    echo "agent-worktree-frontend-deps: incomplete legacy cache was accepted" >&2
    exit 1
fi

complete_root="$WORK_DIR/complete"
write_root_manifest "$complete_root"
write_complete_fixture "$complete_root"
"$CHECKER" --check-frontend-deps "$complete_root"

rm "$complete_root/node_modules/react-dom/client.js"
if "$CHECKER" --check-frontend-deps "$complete_root"; then
    echo "agent-worktree-frontend-deps: corrupted React DOM package was accepted" >&2
    exit 1
fi

echo "agent-worktree-frontend-deps: dependency readiness checks passed"
