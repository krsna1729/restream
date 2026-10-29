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
import argparse, json, math, os, re, signal, statistics, subprocess, sys, threading, time, urllib.request, http.cookiejar
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HARNESS = ["cargo", "xtask", "harness"]


def pinned_restream_bin():
    """Path to the restream-harness-pinned argv0 alias of the xtask binary."""
    subprocess.run(["cargo", "build", "--quiet", "--package", "xtask"], cwd=ROOT, check=True)
    alias = ROOT / ".local/bin/restream-harness-pinned"
    alias.parent.mkdir(parents=True, exist_ok=True)
    alias.unlink(missing_ok=True)
    alias.symlink_to(ROOT / "target/debug/xtask")
    return str(alias)
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
        "rust_profile": "bench (target/bench; cargo xtask build-bench)",
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
    def __init__(self, work_dir, out_path, interval=1.0, attribution=False):
        super().__init__(daemon=True)
        self.interval = interval
        self.work_dir, self.out_path, self.stop = Path(work_dir), Path(out_path), threading.Event()
        self.port = self.pid = None
        self.cookie = None
        self.attribution = attribution
        self.process_gone_at = self.finished_at = self.failure = None

    def outcome(self, stop_requested_at):
        return {"process_gone_at": self.process_gone_at, "finished_at": self.finished_at,
                "failure": self.failure, "stop_requested_at": stop_requested_at,
                "joined": not self.is_alive()}

    def process_exists(self):
        return Path(f"/proc/{self.pid}").exists()

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

    log_name = "restream.log"

    def discover(self):
        log = self.work_dir / self.log_name
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

    def attribution_facts(self):
        """Read-only census; missing evidence stays explicit, never becomes zero."""
        base = Path(f"/proc/{self.pid}")
        facts = {}
        for name in ("smaps_rollup", "smaps"):
            try:
                facts[name] = (base / name).read_text()
            except OSError as exc:
                facts[name + "_error"] = str(exc)
        try:
            facts["threads"] = {
                p.name: (p / "comm").read_text().strip() for p in (base / "task").iterdir()
            }
            facts["socket_fds"] = {
                p.name: os.readlink(p) for p in (base / "fd").iterdir()
                if os.readlink(p).startswith("socket:")
            }
            facts["udp"] = (base / "net/udp").read_text()
            facts["udp6"] = (base / "net/udp6").read_text()
        except OSError as exc:
            facts["census_error"] = str(exc)
        try:
            facts["telemetry"] = self.http("/api/v1/engine/telemetry")
        except Exception as exc:
            facts["telemetry_error"] = str(exc)
        return facts

    def run(self):
        try:
            self.sample()
        except Exception as exc:
            self.failure = str(exc)
        finally:
            self.finished_at = time.time()

    def sample(self):
        while not self.stop.is_set() and not self.discover():
            time.sleep(0.25)
        with self.out_path.open("w") as fh:
            while not self.stop.is_set():
                t0 = time.time()
                rec = {"t": t0}
                try:
                    rec["proc"] = self.proc()
                    if self.attribution:
                        rec["attribution"] = self.attribution_facts()
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
                    if not self.process_exists():
                        self.process_gone_at = time.time()
                        break
                fh.write(json.dumps(rec) + "\n")
                fh.flush()
                self.stop.wait(max(0.0, self.interval - (time.time() - t0)))


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
    p = subprocess.run([*HARNESS, "resource-sweep", "--", "--no-netns"],
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


def end_state(out, r):
    """(outputs with bytesOut>0 at the end, outputs whose bytesOut advanced across the steady window)."""
    path = out / f"{r['variant']}-{r['outputs']}-{r['rep']}.samples.jsonl"
    if not path.exists():
        return None, None
    samples = load_samples(path)
    t_all, window = steady_window(samples, r["outputs"], 8)
    # The final sample can be taken while Restream shuts down (no output rows).
    populated = [s for s in samples if s.get("outputs")]
    last = populated[-1]["outputs"] if populated else {}
    window = [s for s in window if s.get("outputs")]
    progressing = sum(1 for o in last.values() if o["bytesOut"] > 0)
    adv = None
    if window:
        first = window[0].get("outputs", {})
        adv = sum(1 for oid, o in window[-1]["outputs"].items() if o["bytesOut"] > first.get(oid, {}).get("bytesOut", 0))
    return progressing, adv


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
            # The baseline is reported from EVERY repetition (failed ones included):
            # dropping its collapses would hide the very behavior being compared.
            use = [r for r in rs if r.get("cpu_pct") is not None]
            ends = [end_state(out, r) for r in rs]
            fp = lambda r, k: ((r.get("first_progress") or {}).get("firstProgressMs") or {}).get(k)
            vals = lambda k: [r.get(k) for r in use]
            table[f"{v}-{n}"] = {
                "runs": len(rs), "runs_passing_all_gates": sum(1 for r in rs if r["passed"]),
                "outputs_progressing_at_end": [e[0] for e in ends],
                "outputs_advancing_in_window": [e[1] for e in ends],
                "cpu_pct": {"median": med(vals("cpu_pct")), "min": min(vals("cpu_pct"), default=None), "max": max(vals("cpu_pct"), default=None)},
                "rss_mb": {"median": med(vals("rss_mb")), "min": min(vals("rss_mb"), default=None), "max": max(vals("rss_mb"), default=None)},
                "threads_max": max(vals("threads"), default=None), "fds_max": max(vals("fds"), default=None),
                "first_progress_ms": {k: med([fp(r, k) for r in rs]) for k in ("p50", "p95", "max")},
                "first_progress_ms_per_run_p95": [fp(r, "p95") for r in rs],
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
                                "first_progress_p95_regression(>max(1.25x,+1s))": fp_bad,
                                "baseline_operated_normally": b["runs_passing_all_gates"] > 0}
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
                                 rx_truncated=sum(f["rx_truncated"] or 0 for f in fams),
                                 max_service_us=max(f["max_service_us"] or 0 for f in fams),
                                 caller_expired=sum(f["caller_expired"] or 0 for f in fams))
    summary = {"table": table, "baseline_vs_candidate_verdicts": verdicts, "candidate_owner": owner}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


# ------------------------------------------------------------------- burst
def cmd_burst(args):
    """100+ direct SRT outputs created CONCURRENTLY against one pinned shard."""
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    tag = args.tag
    if stray_processes():
        print("ABORT: stray processes", file=sys.stderr)
        sys.exit(3)
    work = ROOT / ".local/artifacts/srt-final" / tag
    subprocess.run(["rm", "-rf", str(work)])
    work.mkdir(parents=True)
    real = ROOT / "target/bench/restream"
    env = dict(os.environ, RESTREAM_BIN=pinned_restream_bin(),
               REAL_RESTREAM=str(real), PIN_CPUS=args.pin_cpus, MSR_PEER="sink",
               RESOURCE_SWEEP_SCENARIOS="egress-growth-source-srt", RESOURCE_SWEEP_EGRESS_COUNTS=str(args.outputs),
               RESOURCE_SWEEP_SAMPLE_SECS="2", RESOURCE_SWEEP_SETTLE_SECS="20", RESOURCE_SWEEP_BURST="1",
               RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS="90", BENCH_BUILD="never", WORK_DIR=str(work))
    # NOTE: SRT egress floors its shard count at 2 (`HostCapacity::egress_shards_per_pool`
    # clamps to 2..=8 and RESTREAM_EGRESS_SHARDS does not apply to the SRT
    # profile), so a pinned run is ONE CPU but TWO shards/Owners.
    if args.connect_concurrency:
        env["RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY"] = str(args.connect_concurrency)
    sampler = Sampler(work, work / "samples.jsonl", interval=0.1)
    sampler.start()
    p = subprocess.run([*HARNESS, "resource-sweep", "--", "--no-netns"],
                       env=env, cwd=ROOT, capture_output=True, text=True, timeout=900)
    sampler.stop.set(); sampler.join(timeout=5)
    log = re.sub(r"\x1b\[[0-9;]*m", "", (work / "restream.log").read_text(errors="ignore"))
    samples = load_samples(work / "samples.jsonl")
    rows = []
    for s in samples:
        for sh_ in s.get("shards", []):
            for own in sh_.get("srtOwners", []) or []:
                if own.get("present"):
                    rows.append((s["t"], s, own))
    if not rows:
        print("no owner samples"); sys.exit(2)
    t0 = rows[0][0]
    peak_q = max(max(r[2]["callerQueued"] for r in rows), max(r[2].get("callerQueuedHwm", 0) for r in rows))
    peak_inflight = max(max(r[2]["callerInFlight"] for r in rows), max(r[2].get("callerInFlightHwm", 0) for r in rows))
    q_up = next((r[0] for r in rows if r[2]["callerQueued"] > 0), None)
    q_zero_after = None
    if q_up is not None:
        q_zero_after = next((r[0] for r in rows if r[0] > q_up and r[2]["callerQueued"] == 0), None)
    last = rows[-1][2]
    # TX handshake progress while the pool was queueing
    tx_during = None
    if q_up is not None and q_zero_after is not None:
        a = next(r[2] for r in rows if r[0] >= q_up); b = next(r[2] for r in rows if r[0] >= q_zero_after)
        tx_during = {"tx_packets_delta": b["txPackets"] - a["txPackets"], "service_actions_delta": b["serviceActions"] - a["serviceActions"],
                     "maintenance_actions_delta": b["maintenanceActions"] - a["maintenanceActions"]}
    fp_path = work / f"first-progress-{args.outputs}.json"
    populated = [x for x in samples if x.get("outputs")]
    outs_last = populated[-1]["outputs"] if populated else {}
    res = {
        "tag": tag, "outputs": args.outputs, "pinned_cpus": args.pin_cpus,
        "connect_concurrency": args.connect_concurrency or "default(64)",
        "harness_exit": p.returncode,
        "peak_caller_queued": peak_q, "peak_caller_in_flight": peak_inflight,
        "queue_longest_continuous_us": max(r[2].get("callerQueueLongestUs", 0) for r in rows),
        "queue_first_above_zero_s": None if q_up is None else round(q_up - t0, 2),
        "queue_drained_to_zero_s": None if q_zero_after is None else round(q_zero_after - t0, 2),
        "queue_drain_duration_s": None if (q_up is None or q_zero_after is None) else round(q_zero_after - q_up, 2),
        "tx_while_queued": tx_during,
        "final": {k: last.get(k) for k in ("callerInFlight", "callerQueued", "callerExpired", "callerFailed", "callerCancelled",
                                          "serviceActions", "maintenanceActions", "txHighWater", "txExhaustions", "protocolOutputFailures", "faulted")},
        "outputs_with_progress": sum(1 for o in outs_last.values() if o["bytesOut"] > 0),
        "first_progress": json.loads(fp_path.read_text()) if fp_path.exists() else None,
        "no_stalled_log": "no progress (stalled)" not in log,
        "pool_full_refusals": log.count("caller pool and its queue are full"),
        "outputs_with_retry": sum(1 for o in outs_last.values() if o.get("retryAttempts", 0) > 0),
        "shards_with_owner": sorted({sh_["shardIndex"] for s in samples for sh_ in s.get("shards", []) if any(o.get("present") for o in sh_.get("srtOwners", []) or [])}),
    }
    res["passed"] = (p.returncode == 0 and res["outputs_with_progress"] >= args.outputs and last["callerQueued"] == 0
                     and last["callerInFlight"] == 0 and last["callerExpired"] == 0 and last["callerFailed"] == 0
                     and res["pool_full_refusals"] == 0 and (peak_q > 0 or args.allow_no_queue))
    (out / f"{tag}.json").write_text(json.dumps(res, indent=2) + "\n")
    (out / f"{tag}.samples.jsonl").write_text((work / "samples.jsonl").read_text())
    print(json.dumps({k: res[k] for k in ("tag", "passed", "peak_caller_queued", "peak_caller_in_flight", "queue_longest_continuous_us", "pool_full_refusals", "outputs_with_retry", "outputs_with_progress", "final", "tx_while_queued")}, indent=1))


# -------------------------------------------------------------- slow peer
def cmd_slow(args):
    """srt.slow-peer harness mode: one paused-application receiver among healthy outputs."""
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    if stray_processes():
        print("ABORT: stray processes", file=sys.stderr)
        sys.exit(3)
    work = ROOT / ".local/artifacts/srt-final" / args.tag
    subprocess.run(["rm", "-rf", str(work)])
    work.mkdir(parents=True)
    real = ROOT / "target/bench/restream"
    env = dict(os.environ, BENCH_BUILD="never", WORK_DIR=str(work), SLOW_PEER_HEALTHY=str(args.healthy),
               SLOW_PEER_WATCH_SECS=str(args.watch), SLOW_PEER_LABEL=args.tag)
    if args.control:
        env["SLOW_PEER_PAUSE"] = "0"
    if args.exact_owner:
        env["SLOW_PEER_EXACT_OWNER"] = "1"
    if args.pin_cpus:
        env.update(RESTREAM_BIN=pinned_restream_bin(), REAL_RESTREAM=str(real), PIN_CPUS=args.pin_cpus)
    else:
        env["RESTREAM_BIN"] = str(real)
    sdir = work / "srt.slow-peer"
    sdir.mkdir(parents=True)
    sampler = Sampler(sdir, work / "samples.jsonl")
    sampler.start()
    p = subprocess.run([*HARNESS, "srt.slow-peer", "--", "--no-netns"],
                       env=env, cwd=ROOT, capture_output=True, text=True, timeout=900)
    sampler.stop.set(); sampler.join(timeout=5)
    result_path = sdir / f"{args.tag}.json"
    res = json.loads(result_path.read_text()) if result_path.exists() else {"passed": False, "error": (p.stderr or p.stdout)[-800:]}
    res["harness_exit"] = p.returncode
    res["pinned_cpus"] = args.pin_cpus
    samples = load_samples(work / "samples.jsonl") if (work / "samples.jsonl").exists() else []
    fams = {}
    for s in samples:
        for sh_ in s.get("shards", []):
            for own in sh_.get("srtOwners", []) or []:
                if own.get("present"):
                    fams[f"shard{sh_['shardIndex']}/{own['family']}"] = {k: own.get(k) for k in (
                        "txCapacity", "txHighWater", "txExhaustions", "txInFlight", "txCompletedOk", "faulted", "serviceVisits",
                        "protocolOutputFailures", "callerExpired", "callerFailed", "rxTruncated")}
    res["owner_final"] = fams
    res["shards_with_owner"] = sorted(fams)
    (out / f"{args.tag}.json").write_text(json.dumps(res, indent=2) + "\n")
    (out / f"{args.tag}.samples.jsonl").write_text((work / "samples.jsonl").read_text() if (work / "samples.jsonl").exists() else "")
    print(json.dumps({k: res.get(k) for k in ("passed", "healthyOutputs", "healthyMinAdvancingPerSecond", "healthyWindowStalls",
                                             "healthyRetryOrFailed", "ownerFaulted", "feedRingPayloadBytes", "slowSinkDataEventsDuringPause",
                                             "slowSinkConnectedAtEnd", "shards_with_owner", "error")}, indent=1))


# ------------------------------------------------------------------ frozen
def frozen_sample_errors(samples, attribution):
    errors = []
    if not samples:
        errors.append("no process samples captured")
    for sample in samples:
        if "error" in sample:
            errors.append({"t": sample["t"], "error": sample["error"]})
        if not sample.get("proc", {}).get("rss_kb", 0) > 0:
            errors.append({"t": sample["t"], "error": "RSS unavailable or zero"})
        if attribution:
            facts = sample.get("attribution", {})
            required = ("smaps", "smaps_rollup", "threads", "socket_fds", "udp", "udp6", "telemetry")
            missing = [key for key in required if key not in facts]
            failures = {k: v for k, v in facts.items() if k.endswith("_error")}
            if "memoryAccounting" not in facts.get("telemetry", {}):
                failures["telemetry"] = "retained-ring accounting unavailable"
            for key in ("smaps", "smaps_rollup", "threads", "udp", "udp6"):
                if key in facts and not facts[key]:
                    failures[key] = "empty evidence"
            if missing or failures:
                errors.append({"t": sample["t"], "missing": missing, "errors": failures})
    return errors


FROZEN_TEST = "srt-egress-bounded-rss-under-frozen-destination"
FROZEN_PHASES = ("live-progress", "sigstop", "retry-observed", "output-stopped",
                 "pipeline-deleted", "capture-complete")


def positive_kb(value):
    return type(value) is int and value > 0


def parse_frozen_report(report):
    """Accept only the current harness artifact, never an exit code or legacy keys."""
    errors = []
    if not isinstance(report, dict):
        report = {}
    if report.get("loadError"):
        errors.append(f"frozen artifact unavailable: {report['loadError']}")
    if report.get("mode") != "fault.srt-output-stall" or report.get("passed") is not True:
        errors.append("missing or failed fault.srt-output-stall report")
    tests = report.get("tests")
    tests = tests if isinstance(tests, list) else []
    matches = [test for test in tests if isinstance(test, dict) and test.get("test") == FROZEN_TEST]
    frozen = matches[0] if len(matches) == 1 else {}
    if len(matches) != 1 or frozen.get("passed") is not True:
        errors.append("missing, duplicate or failed named frozen subtest")
    if frozen.get("sigstopOk") is not True or frozen.get("enteredRetryCycle") is not True:
        errors.append("SIGSTOP and retry-cycle observations required")
    measurement = frozen.get("rssMeasurement")
    measurement = measurement if isinstance(measurement, dict) else {}
    if measurement.get("available") is not True or measurement.get("bounded") is not True:
        errors.append("frozen RSS measurement unavailable or unbounded")
    before, after = frozen.get("rssBeforeStallKb"), frozen.get("rssAfterStallKb")
    if (not positive_kb(before) or not positive_kb(after)
            or not positive_kb(measurement.get("beforeKb")) or not positive_kb(measurement.get("afterKb"))):
        errors.append("two positive rssBeforeStallKb/rssAfterStallKb endpoints required")
    elif measurement.get("beforeKb") != before or measurement.get("afterKb") != after:
        errors.append("RSS measurement endpoints disagree with the frozen subtest")
    else:
        growth = max(0, after - before)
        if (type(measurement.get("growthKb")) is not int or type(frozen.get("rssGrowthKb")) is not int
                or measurement.get("growthKb") != growth or frozen.get("rssGrowthKb") != growth
                or measurement.get("limitKb") != 64 * 1024 or growth > 64 * 1024):
            errors.append("RSS growth or fixed 64 MiB bound disagrees with endpoints")
    if measurement.get("beforeError") is not None or measurement.get("afterError") is not None:
        errors.append("RSS endpoint read errors present")
    return {"report_errors": errors, "gate_rssBeforeKb": before, "gate_rssAfterKb": after,
            "gate_rssGrowthKb": frozen.get("rssGrowthKb"), "gate_limit_kb": 64 * 1024,
            "rss_measurement": measurement, "enteredRetryCycle": frozen.get("enteredRetryCycle"),
            "sigstopOk": frozen.get("sigstopOk")}


def finite_timestamp(value):
    return type(value) in (int, float) and math.isfinite(value)


def frozen_capture_errors(samples, phases, sampler, interval=1.0):
    """Two sampling intervals is the maximum gap, including phase boundaries."""
    errors = []
    markers = {}
    for name in FROZEN_PHASES:
        found = [p.get("t") for p in phases if isinstance(p, dict) and p.get("phase") == name]
        if len(found) != 1 or not finite_timestamp(found[0]):
            errors.append(f"missing, duplicate or invalid phase: {name}")
        else:
            markers[name] = found[0]
    if len(markers) != len(FROZEN_PHASES):
        return errors
    if [p.get("phase") for p in phases if isinstance(p, dict) and p.get("phase") in FROZEN_PHASES] != list(FROZEN_PHASES):
        errors.append("frozen phase records are out of order")
    ordered = [markers[name] for name in FROZEN_PHASES]
    if any(a >= b for a, b in zip(ordered, ordered[1:])):
        errors.append("frozen phases are not strictly ordered")
    if markers["output-stopped"] - markers["retry-observed"] < 120:
        errors.append("retry observation window shorter than 120 seconds")
    times = [s.get("t") for s in samples if s.get("proc")]
    if not times or any(not finite_timestamp(t) for t in times):
        errors.append("no valid timestamped process samples")
        return errors
    if any(a >= b for a, b in zip(times, times[1:])):
        errors.append("process sample timestamps are not strictly ordered")
    max_gap = 2 * interval
    for name, timestamp in markers.items():
        if min(abs(t - timestamp) for t in times) > max_gap:
            errors.append(f"no process sample within {max_gap:g}s of {name}")
    if not any(markers["live-progress"] <= t < markers["sigstop"] for t in times):
        errors.append("no live-progress process sample before SIGSTOP")
    capture = [t for t in times if ordered[0] - max_gap <= t <= ordered[-1] + max_gap]
    if any(b - a > max_gap for a, b in zip(capture, capture[1:])):
        errors.append(f"process sampling gap exceeds {max_gap:g}s during capture")
    end = markers["capture-complete"]
    gone = sampler.get("process_gone_at")
    if gone is not None and (not finite_timestamp(gone) or gone < end):
        errors.append("sampled process disappeared before intentional capture completion")
    finished, stopped = sampler.get("finished_at"), sampler.get("stop_requested_at")
    if (sampler.get("joined") is not True or sampler.get("failure") is not None
            or not finite_timestamp(finished) or not finite_timestamp(stopped)
            or finished < end or stopped < end):
        errors.append("sampler failed or stopped before capture completed")
    return errors


def summarize_frozen_run(report, samples, phases, sampler, attribution, harness_exit, processes_after):
    result = parse_frozen_report(report)
    # Errors from an expected process shutdown after this marker are not capture failures.
    complete = [p.get("t") for p in phases if isinstance(p, dict) and p.get("phase") == "capture-complete"]
    end = complete[0] if len(complete) == 1 and finite_timestamp(complete[0]) else float("inf")
    capture = [s for s in samples if finite_timestamp(s.get("t")) and s["t"] <= end]
    result.update(sample_count=len(samples), sample_errors=frozen_sample_errors(capture, attribution),
                  coverage_errors=frozen_capture_errors(samples, phases, sampler),
                  sampler=sampler, phases=phases, harness_exit=harness_exit, processes_after=processes_after)
    result["passed"] = (harness_exit == 0 and not result["report_errors"]
                        and not result["sample_errors"] and not result["coverage_errors"]
                        and not processes_after)
    return result


def run_frozen_harness(command, env, timeout=900, terminate_timeout=5):
    """Own a session so timing out the shell cannot leave its media children behind."""
    with subprocess.Popen(command, env=env, cwd=ROOT, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, start_new_session=True) as process:
        timed_out = False
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                stdout, stderr = process.communicate(timeout=terminate_timeout)
            except subprocess.TimeoutExpired:
                stdout = stderr = None
            finally:
                # Also retire descendants that closed the inherited pipes before exiting.
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if stdout is None:
                stdout, stderr = process.communicate(timeout=terminate_timeout)
        return {"returncode": process.returncode, "stdout": stdout, "stderr": stderr,
                "timed_out": timed_out}


def cmd_frozen(args):
    """fault.srt-output-stall (SIGSTOPped MediaMTX destination) with an RSS time series."""
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    if stray_processes():
        print("ABORT: stray processes", file=sys.stderr)
        sys.exit(3)
    work = ROOT / ".local/artifacts/srt-final" / args.tag
    if work.exists():
        raise RuntimeError(f"refusing to overwrite existing campaign: {work}")
    work.mkdir(parents=True)
    real = Path(args.restream_bin).resolve() if args.restream_bin else ROOT / "target/qual-release/restream"
    harness = Path(args.harness_bin).resolve() if args.harness_bin else ROOT / "target/qual-release/test_harness"
    env = dict(os.environ, RESTREAM_BIN=str(real), HARNESS_BIN=str(harness),
               BENCH_BUILD="never", WORK_DIR=str(work))
    sdir = work / "fault.srt-output-stall"
    sdir.mkdir(parents=True)
    sampler = Sampler(sdir, work / "samples.jsonl", attribution=args.attribution)
    sampler.log_name = "restream-frozen-destination.log"
    sampler.start()
    run = {"returncode": None, "stdout": "", "stderr": "", "timed_out": False}
    try:
        run = run_frozen_harness(
            [*HARNESS, "fault.srt-output-stall", "--", "--no-netns"], env)
    except (OSError, subprocess.SubprocessError) as exc:
        run["stderr"] = str(exc)
    finally:
        stop_requested_at = time.time()
        sampler.stop.set()
        sampler.join(timeout=15)
        processes_after = stray_processes()
    (work / "harness.stdout").write_text(run["stdout"])
    (work / "harness.stderr").write_text(run["stderr"])
    samples = load_samples(work / "samples.jsonl") if (work / "samples.jsonl").exists() else []
    series = [(round(x["t"] - samples[0]["t"]), x["proc"]["rss_kb"] // 1024, x["proc"]["threads"], x["proc"]["fds"])
              for x in samples if x.get("proc")]
    art = sdir / "fault.srt-output-stall.json"
    try:
        fault = json.loads(art.read_text())
    except (OSError, ValueError) as exc:
        fault = {"loadError": str(exc)}
    phases_path = sdir / "frozen-phases.jsonl"
    phases = load_samples(phases_path) if phases_path.exists() else []
    rss = [r[1] for r in series]
    n = len(rss)
    def med_of(a, b):
        seg = rss[int(n * a):int(n * b)]
        return statistics.median(seg) if seg else None
    res = {"variant": args.variant, "tag": args.tag,
           "rss_mb_median_by_fifth": [med_of(i / 5, (i + 1) / 5) for i in range(5)],
           "rss_mb_max": max(rss) if rss else None, "threads_max": max((r[2] for r in series), default=None),
           "fds_max": max((r[3] for r in series), default=None), "rss_series_mb_every_10s": series[::10]}
    res.update(summarize_frozen_run(fault, samples, phases, sampler.outcome(stop_requested_at),
                                    args.attribution, run["returncode"], processes_after))
    res["harness_timed_out"] = run["timed_out"]
    if run["timed_out"]:
        res["passed"] = False
    res["attribution_requested"] = args.attribution
    res["binaries"] = {"restream": str(real), "harness": str(harness)}
    res["processes_after"] = processes_after
    (out / f"{args.tag}.json").write_text(json.dumps(res, indent=2) + "\n")
    print(json.dumps({k: v for k, v in res.items() if k != "rss_series_mb_every_10s"}, indent=1))
    if not res["passed"]:
        sys.exit(1)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("env"); e.add_argument("--out", required=True)
    e.add_argument("--candidate", required=True); e.add_argument("--baseline", required=True); e.add_argument("--pin", required=True)
    f = sub.add_parser("fanout"); f.add_argument("--out", required=True)
    f.add_argument("--variant", choices=["baseline", "candidate"], required=True)
    f.add_argument("--outputs", type=int, required=True); f.add_argument("--rep", type=int, required=True)
    f.add_argument("--settle", type=int, default=8); f.add_argument("--sample", type=int, default=30)
    b = sub.add_parser("burst"); b.add_argument("--out", required=True); b.add_argument("--tag", required=True)
    b.add_argument("--outputs", type=int, default=100); b.add_argument("--pin-cpus", default="5")
    b.add_argument("--connect-concurrency", type=int, default=0); b.add_argument("--allow-no-queue", action="store_true")
    w = sub.add_parser("slow"); w.add_argument("--out", required=True); w.add_argument("--tag", required=True)
    w.add_argument("--healthy", type=int, default=10); w.add_argument("--watch", type=int, default=30)
    w.add_argument("--pin-cpus", default=""); w.add_argument("--control", action="store_true")
    w.add_argument("--exact-owner", action="store_true")
    z = sub.add_parser("frozen"); z.add_argument("--out", required=True); z.add_argument("--tag", required=True)
    z.add_argument("--variant", choices=["baseline", "candidate"], required=True,
                   help="report label only; binaries are selected by explicit paths or release defaults")
    z.add_argument("--restream-bin"); z.add_argument("--harness-bin")
    z.add_argument("--attribution", action="store_true",
                   help="sample smaps, thread/socket census and retained feed/transport telemetry")
    s = sub.add_parser("summarize"); s.add_argument("--out", required=True)
    args = ap.parse_args()
    {"env": cmd_env, "fanout": cmd_fanout, "burst": cmd_burst, "slow": cmd_slow, "frozen": cmd_frozen, "summarize": cmd_summarize}[args.cmd](args)


if __name__ == "__main__":
    main()
