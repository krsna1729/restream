#!/usr/bin/env python3
"""Tests for the WI2.5 measurement machinery (gates and rate derivation)."""
import copy, importlib.util, os, signal, sys, tempfile, unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("qual", Path(__file__).with_name("srt_final_qual.py"))
qual = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qual)


def owner(t, visits, tx, done, in_flight=0, cap=16, **kw):
    o = {"family": "v4", "present": True, "txCapacity": cap, "txInFlight": in_flight, "txPackets": tx,
         "txCompletedOk": done, "serviceVisits": visits, "serviceActions": visits * 2, "maintenanceActions": 0,
         "serviceDurationSumUs": visits * 20, "serviceBudgetExhausted": 0, "faulted": False, "rxTruncated": 0,
         "rxRingDropped": 0, "managedRx": False, "callerQueued": 0, "callerInFlight": 0, "protocolOutputFailures": 0}
    o.update(kw)
    return o


def sample(t, owners, outputs=3, sent=1000):
    return {"t": t, "proc": {"rss_kb": 100000, "pss_kb": 90000, "threads": 20, "fds": 40, "cpu_jiffies": int(t * 50)},
            "shards": [{"shardIndex": 0, "srtOwners": [owners], "queueOverflows": 0}],
            "outputs": {f"o{i}": {"bytesOut": sent + int(t * 100), "retryAttempts": 0} for i in range(outputs)}}


class Gates(unittest.TestCase):
    def series(self, make):
        return [sample(float(i), make(i)) for i in range(60)]

    def test_healthy_run_passes_every_gate(self):
        s = self.series(lambda i: owner(i, i * 600, i * 700, i * 700, in_flight=3))
        res = qual.analyse(s, 3, 8, 30, "")
        self.assertTrue(all(res["gates"].values()), res["gates"])
        self.assertAlmostEqual(res["owner_families"]["shard0/v4"]["service_visits_per_s"], 600, delta=1)

    def test_full_lanes_without_completions_while_visiting_is_the_nonidle_spin(self):
        # tx lanes full (16/16), no completion progress, visits keep climbing fast
        s = self.series(lambda i: owner(i, i * 5000, 100, 100, in_flight=16))
        res = qual.analyse(s, 3, 8, 30, "")
        self.assertFalse(res["gates"]["no_nonidle_driver_spin"])

    def test_high_visit_rate_with_real_transport_progress_is_not_flagged(self):
        s = self.series(lambda i: owner(i, i * 50000, i * 9000, i * 9000, in_flight=15))
        res = qual.analyse(s, 3, 8, 30, "")
        self.assertTrue(res["gates"]["no_nonidle_driver_spin"])

    def test_stalled_log_and_faults_fail_the_run(self):
        s = self.series(lambda i: owner(i, i * 600, i * 700, i * 700, faulted=True, rxTruncated=1))
        res = qual.analyse(s, 3, 8, 30, "no progress (stalled)")
        self.assertFalse(res["gates"]["no_stalled_log"])
        self.assertFalse(res["gates"]["owner_not_faulted"])
        self.assertFalse(res["gates"]["zero_rx_truncation"])

    def test_partial_connection_is_not_a_sample(self):
        s = self.series(lambda i: owner(i, i * 600, i * 700, i * 700))
        res = qual.analyse(s, 5, 8, 30, "")  # five outputs requested, three exist
        self.assertFalse(res["gates"]["steady_window_captured"])


class FrozenSampling(unittest.TestCase):
    def test_missing_or_zero_rss_is_not_a_valid_measurement(self):
        self.assertTrue(qual.frozen_sample_errors([], False))
        for proc in ({}, {"rss_kb": 0}):
            self.assertTrue(qual.frozen_sample_errors([{"t": 1, "proc": proc}], False))
        self.assertFalse(qual.frozen_sample_errors([{"t": 1, "proc": {"rss_kb": 123}}], False))

    def test_requested_attribution_cannot_silently_lose_evidence(self):
        sample = {"t": 1, "proc": {"rss_kb": 123}}
        self.assertTrue(qual.frozen_sample_errors([sample], True))
        sample["attribution"] = {key: {} for key in
                                ("smaps", "smaps_rollup", "threads", "socket_fds", "udp", "udp6", "telemetry")}
        self.assertTrue(qual.frozen_sample_errors([sample], True))
        sample["attribution"] = {
            "smaps": "Rss: 123 kB", "smaps_rollup": "Rss: 123 kB",
            "threads": {"1": "restream"}, "socket_fds": {},
            "udp": "header", "udp6": "header",
            "telemetry": {"memoryAccounting": {"retainedPayloadBytes": 0}},
        }
        self.assertFalse(qual.frozen_sample_errors([sample], True))
        sample["attribution"]["smaps_error"] = "permission denied"
        self.assertTrue(qual.frozen_sample_errors([sample], True))


class FrozenAcceptance(unittest.TestCase):
    def setUp(self):
        self.report = {
            "mode": "fault.srt-output-stall", "passed": True,
            "tests": [{
                "test": qual.FROZEN_TEST, "passed": True,
                "sigstopOk": True, "enteredRetryCycle": True,
                "rssBeforeStallKb": 100000, "rssAfterStallKb": 105000, "rssGrowthKb": 5000,
                "rssMeasurement": {"available": True, "bounded": True,
                                   "beforeKb": 100000, "afterKb": 105000, "growthKb": 5000,
                                   "limitKb": 65536, "beforeError": None, "afterError": None},
            }],
        }
        self.samples = [sample(float(t), owner(t, 1, 1, 1)) for t in range(142)]
        self.phases = [{"phase": name, "t": t} for name, t in zip(
            qual.FROZEN_PHASES, (2, 10, 15, 135, 138, 141))]
        self.sampler = {"process_gone_at": 142, "finished_at": 143,
                        "stop_requested_at": 150, "joined": True, "failure": None}

    def summarize(self, **overrides):
        args = dict(report=self.report, samples=self.samples, phases=self.phases,
                    sampler=self.sampler, attribution=False, harness_exit=0, processes_after=[])
        args.update(overrides)
        return qual.summarize_frozen_run(**args)

    def test_corrected_harness_report_passes_and_projects_the_real_endpoint_keys(self):
        result = self.summarize()
        self.assertTrue(result["passed"], result)
        self.assertEqual(result["gate_rssBeforeKb"], 100000)
        self.assertEqual(result["gate_rssAfterKb"], 105000)
        self.assertFalse(self.summarize(harness_exit=1)["passed"])
        self.assertFalse(self.summarize(processes_after=["owned task process"])["passed"])

    def test_missing_or_failed_named_artifact_cannot_pass_on_exit_zero(self):
        for report in (None, {}, {"mode": "fault.srt-output-stall", "passed": True, "tests": []}):
            with self.subTest(report=report):
                self.assertFalse(self.summarize(report=report)["passed"])
        for scope in (self.report, self.report["tests"][0]):
            scope["passed"] = False
            self.assertFalse(self.summarize()["passed"])
            scope["passed"] = True
        duplicate = copy.deepcopy(self.report)
        duplicate["tests"].append(copy.deepcopy(duplicate["tests"][0]))
        self.assertFalse(self.summarize(report=duplicate)["passed"])
        for flag in ("sigstopOk", "enteredRetryCycle"):
            report = copy.deepcopy(self.report)
            report["tests"][0][flag] = False
            self.assertFalse(self.summarize(report=report)["passed"])
        self.report["tests"][0]["test"] = "different-subtest"
        self.assertFalse(self.summarize()["passed"])

    def test_null_zero_missing_or_legacy_endpoints_are_rejected(self):
        for key in ("rssBeforeStallKb", "rssAfterStallKb"):
            for value in (None, 0, -1, True, "100000"):
                report = copy.deepcopy(self.report)
                report["tests"][0][key] = value
                with self.subTest(key=key, value=value):
                    self.assertFalse(self.summarize(report=report)["passed"])
            report = copy.deepcopy(self.report)
            report["tests"][0].pop(key)
            self.assertFalse(self.summarize(report=report)["passed"])
        legacy = copy.deepcopy(self.report)
        frozen = legacy["tests"][0]
        frozen["rssBeforeKb"] = frozen.pop("rssBeforeStallKb")
        frozen["rssAfterKb"] = frozen.pop("rssAfterStallKb")
        self.assertFalse(self.summarize(report=legacy)["passed"])

    def test_measurement_must_be_available_bounded_and_consistent(self):
        changes = (("available", None), ("available", False), ("bounded", False),
                   ("beforeKb", 0), ("afterKb", None), ("growthKb", 1),
                   ("limitKb", 100000), ("beforeError", "unreadable"))
        for key, value in changes:
            report = copy.deepcopy(self.report)
            report["tests"][0]["rssMeasurement"][key] = value
            with self.subTest(key=key, value=value):
                self.assertFalse(self.summarize(report=report)["passed"])
        for measurement in (None, {}):
            report = copy.deepcopy(self.report)
            report["tests"][0]["rssMeasurement"] = measurement
            self.assertFalse(self.summarize(report=report)["passed"])

    def test_each_phase_and_the_full_retry_window_must_be_captured(self):
        for missing in qual.FROZEN_PHASES:
            phases = [p for p in self.phases if p["phase"] != missing]
            with self.subTest(missing=missing):
                self.assertFalse(self.summarize(phases=phases)["passed"])
        phases = copy.deepcopy(self.phases)
        phases[3]["t"] = 134
        self.assertFalse(self.summarize(phases=phases)["passed"])
        phases = copy.deepcopy(self.phases)
        phases[1]["t"] = 1
        self.assertFalse(self.summarize(phases=phases)["passed"])
        self.assertFalse(self.summarize(phases=list(reversed(self.phases)))["passed"])

    def test_late_discovery_and_truncated_capture_do_not_pass(self):
        for samples in (self.samples[-1:], self.samples[10:], self.samples[:100],
                        [s for s in self.samples if not 45 <= s["t"] <= 50],
                        [s for s in self.samples if not 7 <= s["t"] <= 13]):
            with self.subTest(sample_count=len(samples)):
                self.assertFalse(self.summarize(samples=samples)["passed"])

    def test_sampler_must_finish_and_join_after_capture(self):
        for field, value in (("joined", False), ("finished_at", None), ("finished_at", 140),
                             ("stop_requested_at", 140), ("failure", "sampler crashed")):
            sampler = dict(self.sampler, **{field: value})
            with self.subTest(field=field, value=value):
                self.assertFalse(self.summarize(sampler=sampler)["passed"])

    def test_process_disappearance_before_intentional_stop_fails(self):
        self.sampler["process_gone_at"] = 140
        result = self.summarize()
        self.assertFalse(result["passed"], result)
        self.assertIn("disappeared", " ".join(result["coverage_errors"]))

    def test_expected_shutdown_after_capture_is_not_a_measurement_failure(self):
        self.samples.append({"t": 142, "error": "process exited"})
        self.assertTrue(self.summarize()["passed"])
        self.sampler["failure"] = "sampler crashed"
        self.assertFalse(self.summarize()["passed"])


class FrozenProcessLifecycle(unittest.TestCase):
    def test_sampler_preserves_process_disappearance_in_its_outcome(self):
        with tempfile.TemporaryDirectory() as directory:
            sampler = qual.Sampler(directory, Path(directory) / "samples.jsonl")
            sampler.discover = lambda: True
            sampler.process_exists = lambda: False
            def gone():
                raise FileNotFoundError("process exited")
            sampler.proc = gone
            sampler.run()
            outcome = sampler.outcome(qual.time.time())
            self.assertIsNotNone(outcome["process_gone_at"])
            self.assertIsNotNone(outcome["finished_at"])
            self.assertIsNone(outcome["failure"])
            self.assertTrue(outcome["joined"])

    def test_timeout_kills_the_wrapper_and_its_signal_resistant_child(self):
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "pids"
            child = "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(60)"
            parent = ("import os,subprocess,sys,time; "
                      f"child=subprocess.Popen([sys.executable,'-c',{child!r}]); "
                      f"open({str(pid_file)!r},'w').write(str(os.getpid())+' '+str(child.pid)); "
                      "time.sleep(60)")
            try:
                result = qual.run_frozen_harness(
                    [sys.executable, "-c", parent], dict(os.environ), timeout=1, terminate_timeout=0.2)
                self.assertTrue(result["timed_out"])
                self.assertNotEqual(result["returncode"], 0)
                self.assertIn("ready", result["stdout"])
                parent_pid, child_pid = map(int, pid_file.read_text().split())
                for pid in (parent_pid, child_pid):
                    status = Path(f"/proc/{pid}/status")
                    if status.exists():
                        self.assertRegex(status.read_text(), r"State:\s+Z")
            finally:
                if pid_file.exists():
                    parent_pid = int(pid_file.read_text().split()[0])
                    try:
                        os.killpg(parent_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == "__main__":
    unittest.main()
