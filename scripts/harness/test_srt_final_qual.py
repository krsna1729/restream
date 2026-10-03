#!/usr/bin/env python3
"""Tests for the WI2.5 measurement machinery (gates and rate derivation)."""
import importlib.util, unittest
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


if __name__ == "__main__":
    unittest.main()
