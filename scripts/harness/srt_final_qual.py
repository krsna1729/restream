#!/usr/bin/env python3
"""Work Item 2.5 measurement driver: SRT Compio egress final qualification.

Drives the existing `resource-sweep` harness (MSR_PEER=sink, scenario
egress-growth-source-srt) one FRESH process per point, samples the Restream
process and its low-cardinality SRT Owner metrics once per second, and derives
the gates and rates the qualification needs. It changes no product behavior.

Subcommands:
  env                         write environment.json
  fanout   --variant V --outputs N --rep R
  summarize                   aggregate every fanout run into summary.json/md
"""
import argparse, json, os, re, statistics, subprocess, sys, threading, time, urllib.request, http.cookiejar
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PASSWORD = os.environ.get("RESTREAM_INITIAL_ADMIN_PASSWORD", "restream-local-harness-password")
CLK = os.sysconf("SC_CLK_TCK")


def sh(cmd, **kw):
    return subprocess.run(cmd, shell=isinstance(cmd, str), capture_output=True, text=True, **kw).stdout.strip()


def stray_processes():
    out = sh("pgrep -a -x restream; pgrep -a -x mediamtx; pgrep -a -x ffmpeg; pgrep -a -x test_harness")
    return [l for l in out.splitlines() if l.strip()]


# --------------------------------------------------------------------- env
def cmd_env(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    lscpu = sh("lscpu")
    def field(name):
        m = re.search(rf"^{name}:\s+(.*)$", lscpu, re.M)
        return m.group(1).strip() if m else None
    gov = None
    p = Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    if p.exists():
        gov = p.read_text().strip()
    cpu_max = Path("/sys/fs/cgroup/cpu.max")
    env = {
        "cpu_model": field("Model name"),
        "logical_cpus": os.cpu_count(),
        "physical_cores": field(r"Core\(s\) per socket"),
        "sockets": field(r"Socket\(s\)"),
        "effective_cpus_affinity": len(os.sched_getaffinity(0)),
        "cgroup_cpu_max": cpu_max.read_text().strip() if cpu_max.exists() else None,
        "memory_total_gib": round(int(re.search(r"MemTotal:\s+(\d+)", Path("/proc/meminfo").read_text()).group(1)) / 1048576, 1),
        "kernel": sh("uname -sr"),
        "arch": sh("uname -m"),
        "governor": gov or "not readable (VM)",
        "rust_profile": "bench (target/bench; scripts/build/bench-harness.sh)",
        "candidate_sha": args.candidate,
        "baseline_sha": args.baseline,
        "srt_rs_pin_candidate": args.pin,
        "peer_mode": "MSR_PEER=sink (harness-native in-process SRT sink)",
        "scenario": "egress-growth-source-srt (h264 SRT ingest 1.5M fixture, source-copy SRT outputs)",
        "srt_latency": "harness default (HARNESS_SRT_LATENCY_US)",
        "restream_env": {k: v for k, v in os.environ.items() if k.startswith(("RESTREAM_EGRESS_", "RESTREAM_SRT_"))},
        "stray_processes_now": stray_processes(),
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    (out / "environment.json").write_text(json.dumps(env, indent=2) + "\n")
    print(json.dumps(env, indent=2))


# ----------------------------------------------------------------- sampler
class Sampler(threading.Thread):
    def __init__(self, work_dir, out_path):
        super().__init__(daemon=True)
        self.work_dir, self.out_path, self.stop = Path(work_dir), Path(out_path), threading.Event()
        self.port = self.pid = None
        self.cookie = None

    def http(self, path, post=None):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=post, method="POST" if post else "GET")
        if post:
            req.add_header("Content-Type", "application/json")
        if self.cookie:
            req.add_header("Cookie", self.cookie)
        with urllib.request.urlopen(req, timeout=5) as resp:
            body = resp.read()
            if post:
                sc = resp.headers.get("Set-Cookie")
                if sc:
                    self.cookie = sc.split(";")[0]
            return json.loads(body) if body else {}

    def discover(self):
        log = self.work_dir / "restream.log"
        if not log.exists():
            return False
        m = re.search(r"http_port\D*(\d+)", re.sub(r"\x1b\[[0-9;]*m", "", log.read_text(errors="ignore")))
        if not m:
            return False
        self.port = int(m.group(1))
        out = sh(f"ss -ltnpH 'sport = :{self.port}'")
        pm = re.search(r"pid=(\d+)", out)
        if not pm:
            return False
        self.pid = int(pm.group(1))
        try:
            self.http("/api/v1/auth/login", json.dumps({"password": PASSWORD}).encode())
        except Exception:
            self.pid = None
            return False
        return True

    def proc(self):
        base = Path(f"/proc/{self.pid}")
        st = (base / "status").read_text()
        stat = (base / "stat").read_text().rsplit(")", 1)[1].split()
        pss = None
        try:
            m = re.search(r"Pss:\s+(\d+)", (base / "smaps_rollup").read_text())
            pss = int(m.group(1)) if m else None
        except Exception:
            pass
        return {
            "rss_kb": int(re.search(r"VmRSS:\s+(\d+)", st).group(1)),
            "pss_kb": pss,
            "threads": int(re.search(r"Threads:\s+(\d+)", st).group(1)),
            "fds": len(os.listdir(base / "fd")),
            "cpu_jiffies": int(stat[11]) + int(stat[12]),
        }

    def run(self):
        while not self.stop.is_set() and not self.discover():
            time.sleep(0.25)
        with self.out_path.open("w") as fh:
            while not self.stop.is_set():
                t0 = time.time()
                rec = {"t": t0}
                try:
                    rec["proc"] = self.proc()
                    sysm = self.http("/metrics/system")
                    rec["shards"] = [s for s in sysm.get("egressShards", []) if s.get("protocol") == "srt"]
                    health = self.http("/api/v1/engine/health")
                    outs = {}
                    for pid, pipe in (health.get("pipelines") or {}).items():
                        for oid, o in (pipe.get("outputs") or {}).items():
                            outs[oid] = {
                                "bytesOut": o.get("bytesOut") or (o.get("metrics") or {}).get("bytesOut") or 0,
                                "phase": o.get("phase"), "status": o.get("status"),
                                "retryAttempts": o.get("retryAttempts") or 0,
                                "backpressureReason": o.get("backpressureReason"),
                                "feedLagUnits": o.get("feedLagUnits"),
                            }
                    rec["outputs"] = outs
                except Exception as exc:  # the process may be shutting down
                    rec["error"] = str(exc)
                    if not Path(f"/proc/{self.pid}").exists():
                        break
                fh.write(json.dumps(rec) + "\n")
                fh.flush()
                self.stop.wait(max(0.0, 1.0 - (time.time() - t0)))


# --------------------------------------------------------------- analysis
def load_samples(path):
    return [json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()]


def steady_window(samples, n_outputs, settle):
    """Samples from `settle` seconds after ALL outputs first made progress."""
    t_all = None
    for s in samples:
        outs = s.get("outputs") or {}
        if len(outs) >= n_outputs and all(o["bytesOut"] > 0 for o in outs.values()):
            t_all = s["t"]
            break
    if t_all is None:
        return None, []
    return t_all, [s for s in samples if s["t"] >= t_all + settle]


def shard_family_series(window):
    """{(shard,family): [(t, owner_json)]}"""
    series = {}
    for s in window:
        for sh_ in s.get("shards", []):
            for own in sh_.get("srtOwners", []) or []:
                if own.get("present"):
                    series.setdefault((sh_["shardIndex"], own["family"]), []).append((s["t"], own, sh_))
    return series


def analyse(samples, n_outputs, settle, sample_secs, log_text):
    t_all, window = steady_window(samples, n_outputs, settle)
    res = {"outputs_requested": n_outputs, "gates": {}, "notes": []}
    if not window or window[-1]["t"] - window[0]["t"] < 5:
        res["gates"]["steady_window_captured"] = False
        return res
    window = [s for s in window if s["t"] <= window[0]["t"] + sample_secs]
    dur = window[-1]["t"] - window[0]["t"]
    first, last = window[0], window[-1]
    cpu = (last["proc"]["cpu_jiffies"] - first["proc"]["cpu_jiffies"]) / CLK / dur * 100.0
    res["window_secs"] = round(dur, 1)
    res["cpu_pct"] = round(cpu, 1)
    res["rss_mb"] = round(statistics.median(s["proc"]["rss_kb"] for s in window) / 1024, 1)
    pss = [s["proc"]["pss_kb"] for s in window if s["proc"].get("pss_kb")]
    res["pss_mb"] = round(statistics.median(pss) / 1024, 1) if pss else None
    res["threads"] = max(s["proc"]["threads"] for s in window)
    res["fds"] = max(s["proc"]["fds"] for s in window)
    res["time_to_all_progress_s"] = round(t_all - samples[0]["t"], 1)
    # --- gates
    outs_first, outs_last = first.get("outputs", {}), last.get("outputs", {})
    res["gates"]["all_outputs_connected"] = len(outs_last) >= n_outputs and all(o["bytesOut"] > 0 for o in outs_last.values())
    mono = True
    for oid in outs_last:
        prev = 0
        for s in window:
            v = (s.get("outputs") or {}).get(oid, {}).get("bytesOut", prev)
            if v < prev:
                mono = False
            prev = v
    res["gates"]["bytesOut_monotonic"] = mono
    advancing = all(outs_last[o]["bytesOut"] > outs_first.get(o, {}).get("bytesOut", 0) for o in outs_last)
    res["gates"]["every_output_advanced_in_window"] = advancing
    res["gates"]["no_stalled_log"] = "no progress (stalled)" not in log_text
    res["gates"]["no_panic"] = "panicked" not in log_text
    res["gates"]["no_unexpected_retry"] = all(o.get("retryAttempts", 0) == 0 for o in outs_last.values())
    series = shard_family_series(window)
    fams = {}
    spin_fail = []
    for (shard, fam), pts in series.items():
        a, b = pts[0][1], pts[-1][1]
        d = pts[-1][0] - pts[0][0]
        def dv(k):
            return b.get(k, 0) - a.get(k, 0)
        visits = dv("serviceVisits")
        rec = {
            "service_visits_per_s": visits / d, "protocol_actions_per_s": dv("serviceActions") / d,
            "maintenance_actions_per_s": dv("maintenanceActions") / d,
            "tx_packets_per_s": dv("txPackets") / d, "tx_completions_per_s": dv("txCompletedOk") / d,
            "actions_per_visit": dv("serviceActions") / visits if visits else None,
            "tx_packets_per_visit": dv("txPackets") / visits if visits else None,
            "avg_service_us": dv("serviceDurationSumUs") / visits if visits else None,
            "max_service_us": b.get("serviceDurationMaxUs"),
            "budget_exhausted_per_s": dv("serviceBudgetExhausted") / d,
            "tx_capacity": b.get("txCapacity"), "tx_high_water": b.get("txHighWater"),
            "tx_exhaustions": b.get("txExhaustions"), "tx_in_flight_last": b.get("txInFlight"),
            "rx_packets_per_s": dv("rxPackets") / d, "rx_ring_dropped": b.get("rxRingDropped"),
            "rx_truncated": b.get("rxTruncated"), "managed_rx": b.get("managedRx"),
            "faulted": b.get("faulted"), "protocol_output_failures": b.get("protocolOutputFailures"),
            "tx_failed_sends": b.get("txFailedSends"), "caller_queued_last": b.get("callerQueued"),
            "caller_in_flight_last": b.get("callerInFlight"), "caller_expired": b.get("callerExpired"),
            "caller_failed": b.get("callerFailed"),
        }
        fams[f"shard{shard}/{fam}"] = rec
        # non-idle spin: tx lanes full with no completions while the shard keeps visiting
        stuck = 0
        for i in range(1, len(pts)):
            p, c = pts[i - 1][1], pts[i][1]
            full = c.get("txCapacity") and c.get("txInFlight") == c.get("txCapacity")
            no_comp = c.get("txCompletedOk", 0) == p.get("txCompletedOk", 0)
            visiting = c.get("serviceVisits", 0) - p.get("serviceVisits", 0) > 50
            stuck = stuck + 1 if (full and no_comp and visiting) else 0
            if stuck >= 3:
                spin_fail.append(f"shard{shard}/{fam}")
                break
        if visits > 1000 * d and dv("txPackets") == 0 and dv("txCompletedOk") == 0 and dv("rxPackets") == 0:
            spin_fail.append(f"shard{shard}/{fam}:visits-without-progress")
    res["owner_families"] = fams
    res["gates"]["no_nonidle_driver_spin"] = not spin_fail
    if spin_fail:
        res["notes"].append(f"spin: {spin_fail}")
    if fams:
        res["gates"]["owner_not_faulted"] = not any(f["faulted"] for f in fams.values())
        res["gates"]["zero_protocol_output_failures"] = not any(f["protocol_output_failures"] for f in fams.values())
        res["gates"]["zero_rx_truncation"] = not any(f["rx_truncated"] for f in fams.values())
        res["gates"]["zero_rx_ring_drop_managed"] = not any(f["rx_ring_dropped"] for f in fams.values() if f["managed_rx"])
        res["gates"]["caller_pool_drained"] = all(f["caller_queued_last"] == 0 and f["caller_in_flight_last"] == 0 for f in fams.values())
        res["gates"]["zero_queue_overflow"] = all(s.get("queueOverflows", 0) == 0 for s in last.get("shards", []))
    # scheduler evidence (candidate only fields absent on older builds)
    sch = {}
    fs, ls = {s["shardIndex"]: s for s in first.get("shards", [])}, {s["shardIndex"]: s for s in last.get("shards", [])}
    for idx, b in ls.items():
        a = fs.get(idx, {})
        sch[f"shard{idx}"] = {k: b.get(k, 0) - a.get(k, 0) for k in
                              ("loopIterations", "loopDurationSumUs", "feedWakesUseful", "feedWakesEmpty",
                               "resyncCount", "queueOverflows", "driverBudgetViolations", "driverOverrunUs", "readyVisits")}
        sch[f"shard{idx}"]["readyDepthHwm"] = b.get("readyDepthHwm")
        sch[f"shard{idx}"]["window_s"] = round(dur, 1)
    res["scheduler"] = sch
    res["srt_runtime_io_uring"] = [s.get("srtRuntimeIoUring") for s in last.get("shards", [])]
    res["srt_managed_rx_available"] = [s.get("srtManagedRxAvailable") for s in last.get("shards", [])]
    return res


# ------------------------------------------------------------------ fanout
def cmd_fanout(args):
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    tag = f"{args.variant}-{args.outputs}-{args.rep}"
    strays = stray_processes()
    if strays:
        print("ABORT: stray processes:", strays, file=sys.stderr)
        sys.exit(3)
    work = ROOT / ".local/artifacts/srt-final" / tag
    subprocess.run(["rm", "-rf", str(work)])
    work.mkdir(parents=True)
    restream = ROOT / (".local/worktrees/wi2-before/target/bench/restream" if args.variant == "baseline" else "target/bench/restream")
    env = dict(os.environ,
               RESTREAM_BIN=str(restream), MSR_PEER="sink",
               RESOURCE_SWEEP_SCENARIOS="egress-growth-source-srt", RESOURCE_SWEEP_EGRESS_COUNTS=str(args.outputs),
               RESOURCE_SWEEP_SAMPLE_SECS="2", RESOURCE_SWEEP_SETTLE_SECS=str(args.settle + args.sample),
               RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS="60", BENCH_BUILD="never", WORK_DIR=str(work))
    sampler = Sampler(work, work / "samples.jsonl")
    sampler.start()
    t0 = time.time()
    p = subprocess.run([str(ROOT / "scripts/harness/run.sh"), "resource-sweep", "--", "--no-netns"],
                       env=env, cwd=ROOT, capture_output=True, text=True, timeout=1500)
    sampler.stop.set()
    sampler.join(timeout=5)
    log = re.sub(r"\x1b\[[0-9;]*m", "", (work / "restream.log").read_text(errors="ignore")) if (work / "restream.log").exists() else ""
    samples = load_samples(work / "samples.jsonl") if (work / "samples.jsonl").exists() else []
    res = analyse(samples, args.outputs, args.settle, args.sample, log)
    fp = work / f"first-progress-{args.outputs}.json"
    res["first_progress"] = json.loads(fp.read_text()) if fp.exists() else None
    res.update(variant=args.variant, outputs=args.outputs, rep=args.rep, harness_exit=p.returncode,
               wall_s=round(time.time() - t0, 1), restream_bin=str(restream))
    res["gates"]["harness_completed"] = p.returncode == 0
    res["passed"] = all(res["gates"].values())
    (out / f"{tag}.json").write_text(json.dumps(res, indent=2) + "\n")
    # keep a compact copy of raw samples (downsampled fields only) next to the result
    (out / f"{tag}.samples.jsonl").write_text((work / "samples.jsonl").read_text() if (work / "samples.jsonl").exists() else "")
    fam = {k: (round(v["service_visits_per_s"]), round(v["tx_packets_per_s"])) for k, v in (res.get("owner_families") or {}).items()}
    print(f"{tag}: passed={res['passed']} cpu={res.get('cpu_pct')} rss={res.get('rss_mb')}MB thr={res.get('threads')} "
          f"gates_failed={[k for k,v in res['gates'].items() if not v]} fam(visits/s,tx/s)={fam}")


# ---------------------------------------------------------------- summarize
def med(vals):
    vals = [v for v in vals if v is not None]
    return statistics.median(vals) if vals else None


def cmd_summarize(args):
    out = Path(args.out)
    runs = [json.loads(p.read_text()) for p in sorted(out.glob("*-*-*.json"))
            if re.match(r"(baseline|candidate)-\d+-\d+\.json$", p.name)]
    table = {}
    for v in ("baseline", "candidate"):
        for n in sorted({r["outputs"] for r in runs}):
            rs = [r for r in runs if r["variant"] == v and r["outputs"] == n]
            if not rs:
                continue
            good = [r for r in rs if r["passed"]]
            def col(k, fn=lambda r, k: r.get(k)):
                return [fn(r, k) for r in good]
            fp = lambda r, k: ((r.get("first_progress") or {}).get("firstProgressMs") or {}).get(k)
            table[f"{v}-{n}"] = {
                "runs": len(rs), "passed_runs": len(good),
                "cpu_pct": {"median": med(col("cpu_pct")), "min": min(col("cpu_pct"), default=None), "max": max(col("cpu_pct"), default=None)},
                "rss_mb": {"median": med(col("rss_mb")), "min": min(col("rss_mb"), default=None), "max": max(col("rss_mb"), default=None)},
                "threads": med(col("threads")), "fds": med(col("fds")),
                "first_progress_ms": {k: med([fp(r, k) for r in rs]) for k in ("p50", "p95", "max")},
                "failed_gates": sorted({g for r in rs for g, ok in r["gates"].items() if not ok}),
            }
    verdicts = {}
    for n in (10, 30):
        b, c = table.get(f"baseline-{n}"), table.get(f"candidate-{n}")
        if b and c and b["cpu_pct"]["median"] and c["cpu_pct"]["median"]:
            cpu_ratio = c["cpu_pct"]["median"] / b["cpu_pct"]["median"]
            rss_ratio = c["rss_mb"]["median"] / b["rss_mb"]["median"]
            bp, cp = b["first_progress_ms"]["p95"], c["first_progress_ms"]["p95"]
            fp_bad = bool(bp and cp and cp > max(bp * 1.25, bp + 1000))
            verdicts[str(n)] = {"cpu_ratio": round(cpu_ratio, 3), "cpu_regression(>+10%)": cpu_ratio > 1.10,
                                "rss_ratio": round(rss_ratio, 3), "rss_regression(>+20%)": rss_ratio > 1.20,
                                "first_progress_p95_regression": fp_bad}
    owner = {}
    for n in sorted({r["outputs"] for r in runs if r["variant"] == "candidate"}):
        rs = [r for r in runs if r["variant"] == "candidate" and r["outputs"] == n and r.get("owner_families")]
        fams = [f for r in rs for f in r["owner_families"].values()]
        if fams:
            g = lambda k: med([f.get(k) for f in fams])
            owner[str(n)] = {k: g(k) for k in ("service_visits_per_s", "protocol_actions_per_s", "maintenance_actions_per_s",
                                               "tx_packets_per_s", "tx_completions_per_s", "actions_per_visit",
                                               "tx_packets_per_visit", "avg_service_us", "budget_exhausted_per_s")}
            owner[str(n)].update(tx_high_water=max(f["tx_high_water"] or 0 for f in fams),
                                 tx_exhaustions=sum(f["tx_exhaustions"] or 0 for f in fams),
                                 rx_ring_dropped=sum(f["rx_ring_dropped"] or 0 for f in fams),
                                 rx_truncated=sum(f["rx_truncated"] or 0 for f in fams))
    summary = {"table": table, "baseline_vs_candidate_verdicts": verdicts, "candidate_owner": owner}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("env"); e.add_argument("--out", required=True)
    e.add_argument("--candidate", required=True); e.add_argument("--baseline", required=True); e.add_argument("--pin", required=True)
    f = sub.add_parser("fanout"); f.add_argument("--out", required=True)
    f.add_argument("--variant", choices=["baseline", "candidate"], required=True)
    f.add_argument("--outputs", type=int, required=True); f.add_argument("--rep", type=int, required=True)
    f.add_argument("--settle", type=int, default=8); f.add_argument("--sample", type=int, default=30)
    s = sub.add_parser("summarize"); s.add_argument("--out", required=True)
    args = ap.parse_args()
    {"env": cmd_env, "fanout": cmd_fanout, "summarize": cmd_summarize}[args.cmd](args)


if __name__ == "__main__":
    main()
