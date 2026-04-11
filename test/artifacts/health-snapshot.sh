#!/usr/bin/env bash
set -euo pipefail

API_URL="${API_URL:-http://localhost:3030}"
OUT_DIR="${OUT_DIR:-test/artifacts/runs}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_FILE="$OUT_DIR/health-$TS.json"

mkdir -p "$OUT_DIR"

ok=0
for attempt in 1 2 3 4 5 6 7 8 9 10; do
	if curl -sf "$API_URL/health" | jq '.' > "$OUT_FILE"; then
		ok=1
		break
	fi
	sleep 1
done

if [[ "$ok" -ne 1 ]]; then
	echo "Failed to fetch $API_URL/health after retries"
	exit 1
fi

echo "Saved health snapshot: $OUT_FILE"
echo "Input ON count: $(jq '[.pipelines[] | select(.input.status=="on")] | length' "$OUT_FILE")"
echo "Output ON count: $(jq '[.pipelines[].outputs[] | select(.status=="on")] | length' "$OUT_FILE")"
