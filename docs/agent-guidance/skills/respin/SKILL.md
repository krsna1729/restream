---
name: respin
description: Tear down any running restream/mediamtx/ffmpeg setup, optionally rebuild the bench binary, then spin up a fresh live demo (mediamtx sink + restream server + SRT publisher with the 2v16a multi-track file) and seed a 4-output pipeline. Use when the user says "respin", "restart the setup", "spin up a live stream", "start the demo", or wants the dashboard running with a live feed.
---

# Skill: respin

Tear down any running restream/mediamtx/ffmpeg setup, optionally rebuild the bench binary, then spin up a fresh live demo: mediamtx sink + restream server + SRT publisher sending the 2v16a multi-track test file. Seeds a pipeline with RTMP source, RTMP 720p, SRT source, and SRT 720p outputs.

Use this skill when the user says "respin", "restart the setup", "spin up a live stream", "start the demo", "rebuild and respin", or asks to get the dashboard running with a live feed.

## Usage

`/respin [--build] [--no-seed]`

- `--build`: force rebuild the bench binary before starting (default: rebuild only if source is newer than binary)
- `--no-seed`: start restream and mediamtx but skip pipeline creation and publisher

## Ports (all configurable at top of this skill)

| Service        | Port  | Env var               |
|----------------|-------|-----------------------|
| Dashboard/API  | 39280 | RESTREAM_HTTP_PORT    |
| RTMP ingest    | 30280 | RESTREAM_RTMP_PORT    |
| SRT ingest     | 31280 | RESTREAM_SRT_PORT     |
| mediamtx RTMP  | 33080 | (mediamtx config)     |
| mediamtx SRT   | 34080 | (mediamtx config)     |
| mediamtx HLS   | 35080 | (mediamtx config)     |
| mediamtx API   | 35081 | (mediamtx config)     |

## Steps

### 1. Kill existing processes (always safe to do)

```bash
pkill -x restream 2>/dev/null
pkill -x mediamtx 2>/dev/null
pkill -x ffmpeg 2>/dev/null
sleep 2
```

Never use `pkill -f` — it would match the agent process. Only kill exact names.

Wait for the port to free:
```bash
until ! ss -tlnp | grep -q ':39280'; do sleep 1; done
```

### 2. Rebuild if needed (only after killing live setup)

**Critical: never cargo build while restream/mediamtx/ffmpeg are running.** The debug binary + FFmpeg children + a compiler can push WSL2's Committed_AS past 8 GB causing a kernel panic.

Check if rebuild is needed:
```bash
# Rebuild if --build flag given, or if any src/ file is newer than the binary
NEED_BUILD=0
[[ "${ARGS}" == *--build* ]] && NEED_BUILD=1
[[ -f target/release/restream ]] || NEED_BUILD=1
if [[ $NEED_BUILD -eq 0 ]]; then
  find src/ Cargo.toml -newer target/release/restream 2>/dev/null | grep -q . && NEED_BUILD=1
fi

if [[ $NEED_BUILD -eq 1 ]]; then
  echo "Building bench binary..."
  scripts/resource-limit cargo build --profile bench 2>&1 | tail -3
fi
```

### 3. Write mediamtx config and start mediamtx

```bash
WORK=/tmp/restream-live
mkdir -p "$WORK"

cat > "$WORK/mediamtx.yml" << 'YML'
logLevel: warn
rtmp: yes
rtmpAddress: :33080
srt: yes
srtAddress: :34080
hls: yes
hlsAddress: :35080
webrtc: no
api: yes
apiAddress: :35081
paths:
  all:
YML

mediamtx "$WORK/mediamtx.yml" >> "$WORK/mediamtx.log" 2>&1 &
echo $! > "$WORK/mediamtx.pid"
```

### 4. Start restream bench binary

```bash
RESTREAM_HTTP_PORT=39280 \
RESTREAM_RTMP_PORT=30280 \
RESTREAM_SRT_PORT=31280 \
RESTREAM_DB_PATH="$WORK/restream.db" \
RESTREAM_MEDIA_DIR="$(pwd)/media" \
./target/release/restream >> "$WORK/restream.log" 2>&1 &
RS_PID=$!
echo $RS_PID > "$WORK/restream.pid"

# Wait for healthz (up to 20 s)
for i in $(seq 1 20); do
  curl -sf http://127.0.0.1:39280/healthz >/dev/null 2>&1 && break
  sleep 1
done
```

### 5. Login

```bash
curl -sf -c "$WORK/cookies.txt" -X POST http://127.0.0.1:39280/api/auth/login \
  -H 'Content-Type: application/json' -d '{"password":"admin"}' > /dev/null
```

### 6. Seed pipeline (skip if --no-seed)

Create pipeline with stream key `live`:

```bash
PIPE_RESP=$(curl -sf -b "$WORK/cookies.txt" -X POST http://127.0.0.1:39280/pipelines \
  -H 'Content-Type: application/json' \
  -d '{"name":"2v16a adaptive-ring demo","streamKey":"live"}')
PIPE_ID=$(echo "$PIPE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['pipeline']['id'])")
```

Create and start 4 outputs (loop over name|url|encoding triples):

```bash
for SPEC in \
  "RTMP_source|rtmp://127.0.0.1:33080/live/rtmp-src|source" \
  "RTMP_720p|rtmp://127.0.0.1:33080/live/rtmp-720p|720p" \
  "SRT_source|srt://127.0.0.1:34080?streamid=publish:live/srt-src&pkt_size=1316|source" \
  "SRT_720p|srt://127.0.0.1:34080?streamid=publish:live/srt-720p&pkt_size=1316|720p"; do
  IFS='|' read -r NAME URL ENC <<< "$SPEC"
  OID=$(curl -sf -b "$WORK/cookies.txt" \
    -X POST "http://127.0.0.1:39280/pipelines/$PIPE_ID/outputs" \
    -H 'Content-Type: application/json' \
    --data-raw "{\"name\":\"$NAME\",\"url\":\"$URL\",\"encoding\":\"$ENC\"}" | \
    python3 -c "import sys,json; print(json.load(sys.stdin)['output']['id'])")
  curl -sf -b "$WORK/cookies.txt" \
    -X POST "http://127.0.0.1:39280/pipelines/$PIPE_ID/outputs/$OID/start" > /dev/null
done
```

Start the 2v16a SRT publisher (H.264 1080p30 at stream index 1, all 16 audio tracks):

```bash
ffmpeg -nostdin -hide_banner -loglevel error \
  -re -stream_loop -1 -i media/colorbar-timer-2v16a.mp4 \
  -map 0:1 -map 0:a? -c copy \
  -f mpegts 'srt://127.0.0.1:31280?streamid=publish:live/live&pkt_size=1316' \
  >> "$WORK/ffmpeg-pub.log" 2>&1 &
echo $! > "$WORK/ffmpeg-pub.pid"
```

### 7. Wait for live stream and adaptive ring resize

Poll until ingest is "on":
```bash
for i in $(seq 1 30); do
  STATUS=$(curl -sf -b "$WORK/cookies.txt" http://127.0.0.1:39280/health | \
    python3 -c "import sys,json; h=json.load(sys.stdin); [print(p['input']['status']) for p in h.get('pipelines',{}).values()]" 2>/dev/null)
  [[ "$STATUS" == "on" ]] && break
  sleep 1
done
```

Watch for the adaptive ring resize log line — confirms the 16-audio-track stream was detected and the ring was correctly sized to ~4980 slots (6.0 s headroom):
```bash
timeout 20 grep -m1 'Adaptive ring resize' <(tail -f "$WORK/restream.log") || true
```

### 8. Report status

Query health and ring telemetry, then print a clean summary:

```bash
curl -sf -b "$WORK/cookies.txt" http://127.0.0.1:39280/health | python3 -c "
import sys,json
h=json.load(sys.stdin)
for pid,p in h.get('pipelines',{}).items():
    inp=p['input']
    print(f'  Input: {inp.get(\"status\")} proto={inp.get(\"protocol\")} rx={inp.get(\"bytesReceived\",0)//1024}KB')
    for oid,o in p.get('outputs',{}).items():
        print(f'  {o.get(\"name\",oid):20}: {o.get(\"status\")}')
"

# Ring depth (shows adaptive sizing worked)
PIPE_ID=$(curl -sf -b "$WORK/cookies.txt" http://127.0.0.1:39280/pipelines | \
  python3 -c "import sys,json; [print(p['id']) for p in json.load(sys.stdin).get('pipelines',[])]" | head -1)
curl -sf -b "$WORK/cookies.txt" "http://127.0.0.1:39280/api/v1/pipelines/$PIPE_ID/telemetry" | python3 -c "
import sys,json
t=json.load(sys.stdin)
rb=t.get('sourceRing',{})
cap=rb.get('capacity',0); rate=rb.get('estimatedPktRatePerSec',0); depth=rb.get('bufferDepthSecs')
if cap: print(f'  Ring: {cap} slots, {rate}pkt/s, {depth:.1f}s headroom' if depth else f'  Ring: {cap} slots')
" 2>/dev/null || true
```

Print the final summary box:

```
Dashboard: http://127.0.0.1:39280/
Login:     admin
Ingest:    srt://127.0.0.1:31280  (publish:live/live)
RTMP sink: rtmp://127.0.0.1:33080/live/{rtmp-src, rtmp-720p}
SRT sink:  srt://127.0.0.1:34080  (publish:live/{srt-src, srt-720p})
HLS:       http://127.0.0.1:35080
Logs:      /tmp/restream-live/{restream,mediamtx,ffmpeg-pub}.log
```

## Notes

- **WSL2 build safety:** the skill kills all media processes before any `cargo build` call. Never invert this order.
- **Adaptive ring sizing:** for the 2v16a stream (830 pkt/s), the ring resizes from 1024→4980 slots automatically after probe (~2-3 s). The SRT egress connections that attached before the resize are cancelled and reconnect within ~1 s to the new ring — this is normal and visible in the log as `cancelled N egress(es) for reconnect`.
- **SRT 720p startup:** the SRT 720p output takes ~5-10 s longer than RTMP outputs because it requires the transcoder FFmpeg process to start and produce its first keyframe before the TS mux has data to send.
- **Stale SRT outputs:** if an output shows "stalled" after 30 s, stop and restart it via the API or dashboard — this is a mediamtx connection timing issue, not a restream bug.
- **DB path:** each respin uses a fresh `/tmp/restream-live/restream.db`, so no pipeline config carries over between respins.
