#!/usr/bin/env python3
"""Summarize WI3.7 egress-duty artifacts without changing product policy.

The input tree is produced by ``wi37-capacity.sh``. Valid rows are used for a
small ordinary-least-squares service-demand fit per shard count. If an arm
ends at a receiver limit, the fit is explicitly marked extrapolated; it is a
measurement aid for WI10, not a production shard-count decision.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from pathlib import Path


def finite(value):
    return isinstance(value, (int, float)) and math.isfinite(value)


def fit_line(points):
    if len(points) < 2:
        return {"points": len(points), "slope": None, "intercept": None}
    xbar = statistics.fmean(x for x, _ in points)
    ybar = statistics.fmean(y for _, y in points)
    denominator = sum((x - xbar) ** 2 for x, _ in points)
    if denominator == 0:
        return {"points": len(points), "slope": None, "intercept": None}
    slope = sum((x - xbar) * (y - ybar) for x, y in points) / denominator
    return {
        "points": len(points),
        "slopeUsPerDataPerGbps": slope,
        "interceptUsPerData": ybar - slope * xbar,
    }

def median_field(rows, field):
    values = [row.get(field) for row in rows if finite(row.get(field))]
    return statistics.median(values) if values else None


def load_rows(root):
    rows = []
    for path in sorted(root.rglob("egress-duty.json")):
        try:
            artifact = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        config = artifact.get("config") or {}
        capacity = artifact.get("capacity") or {}
        cpu = artifact.get("cpu") or {}
        if config.get("crypto", "plain") != "plain":
            continue
        shards = config.get("requestedShards")
        outputs = config.get("outputs")
        offered = (capacity.get("offered") or {}).get("payloadGbps")
        service = (capacity.get("serviceSignals") or {}).get("durationSumUs")
        data = (capacity.get("firstData") or {}).get("packets")
        window = (artifact.get("window") or {}).get("observedSecs")
        us_per_data = cpu.get("egressThreadUsPerDataFirst")
        row = {
            "artifact": str(path),
            "shards": shards,
            "outputs": outputs,
            "classification": capacity.get("classification"),
            "apparatusValid": capacity.get("apparatusValid"),
            "offeredPayloadGbps": offered,
            "dataFirst": data,
            "windowSecs": window,
            "serviceDurationSumUs": service,
            "egressThreadUsPerDataFirst": us_per_data,
            "processUsPerDataFirst": cpu.get("processUsPerDataFirst"),
            "hottestShardCpuUtilization": cpu.get("hottestShardCpuUtilization"),
            "shardCpuImbalance": cpu.get("shardCpuImbalance"),
        }
        if finite(data) and finite(window) and window > 0:
            row["dataFirstPerSec"] = data / window
        else:
            row["dataFirstPerSec"] = None
        rows.append(row)
    return rows

def load_crypto_rows(root):
    rows = []
    for path in sorted(root.rglob("egress-duty.json")):
        try:
            artifact = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        config = artifact.get("config") or {}
        crypto = config.get("crypto")
        capacity = artifact.get("capacity") or {}
        if crypto not in ("128", "256") or capacity.get("apparatusValid") is not True:
            continue
        cpu = artifact.get("cpu") or {}
        receiver = artifact.get("receiver") or {}
        data = (capacity.get("firstData") or {}).get("packets")
        service_us = (capacity.get("serviceSignals") or {}).get("durationSumUs")
        row = {
            "artifact": str(path),
            "crypto": crypto,
            "offeredPayloadGbps": (capacity.get("offered") or {}).get("payloadGbps"),
            "dataFirst": data,
            "egressThreadUsPerDataFirst": cpu.get("egressThreadUsPerDataFirst"),
            "processUsPerDataFirst": cpu.get("processUsPerDataFirst"),
            "serviceDurationUsPerDataFirst": (
                service_us / data if finite(service_us) and finite(data) and data > 0 else None
            ),
            "receiverCpuSysMs": receiver.get("cpuSysMs"),
            "receiverCpuUserMs": receiver.get("cpuUserMs"),
        }
        rows.append(row)
    return rows


def summarize(root, target_utilization):
    rows = load_rows(root)
    arms = {}
    for row in rows:
        shards = row.get("shards")
        if not isinstance(shards, int):
            continue
        arms.setdefault(str(shards), []).append(row)

    arm_summaries = {}
    required_shards = []
    for key, arm in sorted(arms.items(), key=lambda item: int(item[0])):
        arm.sort(key=lambda row: row.get("outputs") or 0)
        valid = [
            row
            for row in arm
            if row.get("apparatusValid") is True
            and finite(row.get("offeredPayloadGbps"))
            and finite(row.get("egressThreadUsPerDataFirst"))
        ]
        receiver_capped = any(
            row.get("classification") == "receiver-apparatus-limited" for row in arm
        )
        sender_knee = next(
            (row for row in arm if row.get("classification") == "sender-saturated"),
            None,
        )
        points = [
            (row["offeredPayloadGbps"], row["egressThreadUsPerDataFirst"])
            for row in valid
        ]
        fit = fit_line(points)
        fit["extrapolated"] = bool(receiver_capped and sender_knee is None)
        fit["basis"] = (
            "valid lower cells before receiver apparatus limit"
            if fit["extrapolated"]
            else "valid cells"
        )

        for row in valid:
            demand = row.get("dataFirstPerSec")
            service_us = row.get("egressThreadUsPerDataFirst")
            if not finite(demand) or not finite(service_us):
                continue
            shards_needed = math.ceil(
                demand * service_us / 1_000_000.0 / target_utilization
            )
            required_shards.append(shards_needed)
            row["provisionalRequiredShards"] = shards_needed
            row["targetShardUtilization"] = target_utilization
            row["fixedOverheadGuard"] = "apply per-host fixed overhead and tail guard"

        arm_summaries[key] = {
            "rows": arm,
            "validRows": len(valid),
            "receiverCapped": receiver_capped,
            "senderKnee": sender_knee,
            "serviceDemandFit": fit,
            "medianEgressUsPerDataFirst": median_field(
                valid, "egressThreadUsPerDataFirst"
            ),
            "medianProcessUsPerDataFirst": median_field(
                valid, "processUsPerDataFirst"
            ),
            "medianShardCpuImbalance": median_field(valid, "shardCpuImbalance"),
        }

    shard_comparisons = []
    arm_keys = sorted(arm_summaries, key=int)
    for previous, current in zip(arm_keys, arm_keys[1:]):
        previous_summary = arm_summaries[previous]
        current_summary = arm_summaries[current]
        previous_service = previous_summary["medianEgressUsPerDataFirst"]
        current_service = current_summary["medianEgressUsPerDataFirst"]
        service_change = None
        if finite(previous_service) and finite(current_service) and previous_service:
            service_change = 1.0 - current_service / previous_service
        shard_comparisons.append(
            {
                "fromShards": int(previous),
                "toShards": int(current),
                "egressServiceDemandReduction": service_change,
                "fromMedianProcessUsPerDataFirst": previous_summary[
                    "medianProcessUsPerDataFirst"
                ],
                "toMedianProcessUsPerDataFirst": current_summary[
                    "medianProcessUsPerDataFirst"
                ],
                "fixedOverheadGuard": "accept an extra shard only when capacity/tail gain pays fixed runtime and wakeup cost",
            }
        )

    crypto_rows = load_crypto_rows(root)
    crypto_by_mode = {}
    for row in crypto_rows:
        crypto_by_mode.setdefault(row["crypto"], row)
    crypto_increment = None
    if "128" in crypto_by_mode and "256" in crypto_by_mode:
        aes128 = crypto_by_mode["128"]
        aes256 = crypto_by_mode["256"]
        crypto_increment = {
            "basis": "one bounded apparatus-valid cell per mode; provisional",
            "aes128": aes128,
            "aes256": aes256,
            "aes256MinusAes128": {
                field: aes256[field] - aes128[field]
                for field in (
                    "egressThreadUsPerDataFirst",
                    "processUsPerDataFirst",
                    "serviceDurationUsPerDataFirst",
                    "receiverCpuSysMs",
                    "receiverCpuUserMs",
                )
                if finite(aes128.get(field)) and finite(aes256.get(field))
            },
            "fullMatrix": "not run; expand only if repeated bounded cells show nonlinear behavior",
        }
    else:
        crypto_increment = {
            "basis": "no paired apparatus-valid AES-128/AES-256 cells",
            "aes128": crypto_by_mode.get("128"),
            "aes256": crypto_by_mode.get("256"),
            "aes256MinusAes128": None,
            "fullMatrix": "not run",
        }

    portable = max(required_shards) if required_shards else None
    return {
        "mode": "wi3.7-capacity-analysis",
        "root": str(root),
        "shardComparisons": shard_comparisons,
        "cryptoIncrement": crypto_increment,
        "targetShardUtilization": target_utilization,
        "arms": arm_summaries,
        "portableCoefficient": {
            "provisional": True,
            "requiredShardsUpperBound": portable,
            "equation": "ceil(DATA/event demand * measured service demand / target utilization) + fixed-overhead/tail guard",
            "productionDefault": "unchanged; defer portable coefficient and default choice to WI10",
        },
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root", nargs="?", default=".local/artifacts/wi37-capacity")
    parser.add_argument("--target-utilization", type=float, default=0.8)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if not 0.0 < args.target_utilization <= 1.0:
        parser.error("--target-utilization must be in (0, 1]")
    result = summarize(Path(args.root), args.target_utilization)
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(text)
    print(text, end="")


if __name__ == "__main__":
    main()
