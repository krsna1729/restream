#!/usr/bin/env bash
# Regenerate distribution/docker/restream-seccomp.json from a Moby default
# seccomp profile: baseline + the io_uring delta documented in
# distribution/docker/README.md. Usage: refresh-docker-seccomp.sh <moby-tag>
set -euo pipefail

ROOT="${RESTREAM_REPO_ROOT:-$(git rev-parse --show-toplevel)}"
TAG="${1:?usage: refresh-docker-seccomp.sh <moby-tag, e.g. docker-v29.8.1>}"
OUT_DIR="$ROOT/distribution/docker"
URL="https://raw.githubusercontent.com/moby/moby/${TAG}/vendor/github.com/moby/profiles/seccomp/default.json"

for command in curl python3 sha256sum; do
    command -v "$command" >/dev/null || {
        echo "refresh-docker-seccomp: required command not found: $command" >&2
        exit 1
    }
done

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
curl --fail --silent --show-error --location "$URL" -o "$tmp"
sha="$(sha256sum "$tmp" | cut -d' ' -f1)"

python3 - "$tmp" "$OUT_DIR" "$TAG" <<'PY'
import json, sys
src, out_dir, tag = sys.argv[1:4]
base = json.load(open(src))
if "io_uring" in open(src).read():
    sys.exit("the baseline already mentions io_uring: drop the delta instead of adding a redundant rule")
open(f"{out_dir}/moby-default-seccomp-{tag}.json", "wb").write(open(src, "rb").read())
profile = json.loads(json.dumps(base))
profile["syscalls"].append({
    "names": ["io_uring_enter", "io_uring_register", "io_uring_setup"],
    "action": "SCMP_ACT_ALLOW",
})
open(f"{out_dir}/restream-seccomp.json", "w").write(json.dumps(profile, indent="\t") + "\n")
PY

echo "refresh-docker-seccomp: baseline ${TAG} sha256=${sha}"
echo "refresh-docker-seccomp: update distribution/docker/README.md provenance, the"
echo "refresh-docker-seccomp: BASE path in tests/seccomp_profile.rs, then run the container smoke"
