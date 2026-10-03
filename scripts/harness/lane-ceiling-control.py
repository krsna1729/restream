#!/usr/bin/env python3
"""Lane ceiling control for substrate benchmarks.

The substrate benchmark (harness mode `substrate-pps`) compares UDP submission
mechanisms, but a comparison only means something if the lane itself is not the
ceiling. This control sends the same payload to the same destinations with a
bare blocking `sendto` loop pinned to one CPU and reports pps plus the drain
peer's counters: if a plain syscall loop matches the measured variants, the
result is a lane property, not a submission-API property.

Usage (from a veth/CPU-partitioned lane, with the `udp-drain` peer running):

    taskset -c 0 scripts/harness/lane-ceiling-control.py

Environment: CEILING_DEST_COUNT (default 1000), CEILING_SECS (default 20),
CEILING_PEER_STATE (default http://10.53.0.2:9997/state),
CEILING_DEST_BASE (default 10.53.1.1), CEILING_DEST_PORT (default 9000).
"""

import json, socket, struct, time, urllib.request, os

def state():
    url = os.environ.get("CEILING_PEER_STATE", "http://10.53.0.2:9997/state")
    return json.load(urllib.request.urlopen(url, timeout=5))

base = socket.inet_aton(os.environ.get("CEILING_DEST_BASE", "10.53.1.1"))
b = int.from_bytes(base, "big")
count = int(os.environ.get("CEILING_DEST_COUNT", "1000"))
port = int(os.environ.get("CEILING_DEST_PORT", "9000"))
dests = [(socket.inet_ntoa((b + i).to_bytes(4, "big")), port) for i in range(count)]
payload = bytes((i % 251) for i in range(1316))
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 8 * 1024 * 1024)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)

# warmup
end = time.perf_counter() + 3
i = 0
while time.perf_counter() < end:
    s.sendto(payload, dests[i % count]); i += 1
before, t0 = state(), time.perf_counter()
sends0 = i
end = time.perf_counter() + float(os.environ.get("CEILING_SECS", "20"))
while time.perf_counter() < end:
    s.sendto(payload, dests[i % count]); i += 1
secs = time.perf_counter() - t0
after = state()
sends = i - sends0
print(json.dumps({
    "control": "python-blocking-sendto",
    "destinations": count,
    "cpusAllowedList": sorted(os.sched_getaffinity(0)),
    "secs": round(secs, 3),
    "sends": sends,
    "pps": round(sends / secs, 1),
    "payloadGbitPerSec": round(sends * 1316 * 8 / secs / 1e9, 3),
    "receiverDatagrams": after["datagrams"] - before["datagrams"],
    "receiverRcvbufErrors": after["udpRcvbufErrors"] - before["udpRcvbufErrors"],
    "cpuSecs": round(time.process_time(), 3),
}, indent=1))
