#!/usr/bin/env python3
"""Summarize WI3.7 egress-duty artifacts without changing product policy.

The input tree is produced by ``wi37-capacity.sh``. Apparatus-valid plaintext
rows fit CPU demand directly:

    egress cores = fixed cores/shard * shard count + seconds/DATA * DATA rate

Receiver-capped and sender-saturated rows remain visible but do not drive the
fit. The result is measurement aid for WI10, not a production shard decision.
"""
from __future__ import annotations

import argparse
import json
import math
import re
import statistics
from pathlib import Path


def finite(value):
    return isinstance(value, (int, float)) and math.isfinite(value)


def fit_affine(points):
    """Fit y = intercept + slope*x and report residual quality."""
    if len(points) < 2:
        return {
            "points": len(points),
            "interceptCoreEquivalents": None,
            "secondsPerData": None,
            "residualSumSquares": None,
            "rmseCoreEquivalents": None,
            "rSquared": None,
        }
    xbar = statistics.fmean(x for x, _ in points)
    ybar = statistics.fmean(y for _, y in points)
    denominator = sum((x - xbar) ** 2 for x, _ in points)
    if denominator == 0:
        return {
            "points": len(points),
            "interceptCoreEquivalents": None,
            "secondsPerData": None,
            "residualSumSquares": None,
            "rmseCoreEquivalents": None,
            "rSquared": None,
        }
    slope = sum((x - xbar) * (y - ybar) for x, y in points) / denominator
    intercept = ybar - slope * xbar
    residuals = [y - (intercept + slope * x) for x, y in points]
    residual_sum_squares = sum(value * value for value in residuals)
    total_sum_squares = sum((y - ybar) ** 2 for _, y in points)
    return {
        "points": len(points),
        "interceptCoreEquivalents": intercept,
        "secondsPerData": slope,
        "residualSumSquares": residual_sum_squares,
        "rmseCoreEquivalents": math.sqrt(residual_sum_squares / len(points)),
        "rSquared": (
            1.0 - residual_sum_squares / total_sum_squares
            if total_sum_squares > 0
            else None
        ),
    }


def fit_joint(points):
    """Fit y = fixed_per_shard*n + seconds_per_data*rate, without an intercept."""
    if len(points) < 2:
        return {
            "points": len(points),
            "fixedCorePerShard": None,
            "secondsPerData": None,
            "residualSumSquares": None,
            "rmseCoreEquivalents": None,
            "rSquared": None,
        }
    sum_nn = sum(shards * shards for shards, _, _ in points)
    sum_nr = sum(shards * rate for shards, rate, _ in points)
    sum_rr = sum(rate * rate for _, rate, _ in points)
    sum_ny = sum(shards * cores for shards, _, cores in points)
    sum_ry = sum(rate * cores for _, rate, cores in points)
    determinant = sum_nn * sum_rr - sum_nr * sum_nr
    if determinant == 0:
        return {
            "points": len(points),
            "fixedCorePerShard": None,
            "secondsPerData": None,
            "residualSumSquares": None,
            "rmseCoreEquivalents": None,
            "rSquared": None,
        }
    fixed = (sum_ny * sum_rr - sum_nr * sum_ry) / determinant
    seconds_per_data = (sum_nn * sum_ry - sum_nr * sum_ny) / determinant
    predictions = [
        fixed * shards + seconds_per_data * rate for shards, rate, _ in points
    ]
    residuals = [cores - predicted for predicted, (_, _, cores) in zip(predictions, points)]
    residual_sum_squares = sum(value * value for value in residuals)
    mean_cores = statistics.fmean(cores for _, _, cores in points)
    total_sum_squares = sum((cores - mean_cores) ** 2 for _, _, cores in points)
    return {
        "points": len(points),
        "fixedCorePerShard": fixed,
        "secondsPerData": seconds_per_data,
        "residualSumSquares": residual_sum_squares,
        "rmseCoreEquivalents": math.sqrt(residual_sum_squares / len(points)),
        "rSquared": (
            1.0 - residual_sum_squares / total_sum_squares
            if total_sum_squares > 0
            else None
        ),
    }


def median_field(rows, field):
    values = [row.get(field) for row in rows if finite(row.get(field))]
    return statistics.median(values) if values else None


def selected_artifact_paths(root):
    """Choose one newest artifact per logical cell, including resumable attempts."""
    selected = {}
    for path in root.rglob("egress-duty.json"):
        try:
            artifact = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        config = artifact.get("config") or {}
        crypto = config.get("crypto", "plain")
        shards = config.get("requestedShards")
        outputs = config.get("outputs")
        if not isinstance(shards, int) or not isinstance(outputs, int):
            continue
        key = (crypto, shards, outputs, crypto_repeat(path) if crypto != "plain" else None)
        previous = selected.get(key)
        if previous is None or path.stat().st_mtime_ns >= previous.stat().st_mtime_ns:
            selected[key] = path
    return sorted(selected.values())


def load_rows(root):
    rows = []
    for path in selected_artifact_paths(root):
        try:
            artifact = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        config = artifact.get("config") or {}
        capacity = artifact.get("capacity") or {}
        cpu = capacity.get("cpu") or artifact.get("cpu") or {}
        if config.get("crypto", "plain") != "plain":
            continue
        shards = config.get("requestedShards")
        outputs = config.get("outputs")
        offered = (capacity.get("offered") or {}).get("payloadGbps")
        service = (capacity.get("serviceSignals") or {}).get("durationSumUs")
        data = (capacity.get("firstData") or {}).get("packets")
        window = (artifact.get("window") or {}).get("observedSecs")
        egress_cpu_secs = cpu.get("egressThreadCpuSecs")
        process_us_per_data = cpu.get("processUsPerDataFirst")
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
            "egressThreadCpuSecs": egress_cpu_secs,
            "egressThreadUsPerDataFirst": cpu.get("egressThreadUsPerDataFirst"),
            "processUsPerDataFirst": process_us_per_data,
            "hottestShardCpuUtilization": cpu.get("hottestShardCpuUtilization"),
            "shardCpuImbalance": cpu.get("shardCpuImbalance"),
        }
        if finite(data) and finite(window) and window > 0:
            row["dataRate"] = data / window
        else:
            row["dataRate"] = None
        if finite(egress_cpu_secs) and finite(window) and window > 0:
            row["egressCoreEquivalents"] = egress_cpu_secs / window
        else:
            row["egressCoreEquivalents"] = None
        rows.append(row)
    return rows


def crypto_repeat(path):
    match = re.search(r"repeat-(\d+)", str(path))
    return int(match.group(1)) if match else None


def load_crypto_rows(root):
    rows = []
    for path in selected_artifact_paths(root):
        try:
            artifact = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        config = artifact.get("config") or {}
        crypto = config.get("crypto")
        capacity = artifact.get("capacity") or {}
        if crypto not in ("128", "256") or capacity.get("apparatusValid") is not True:
            continue
        cpu = capacity.get("cpu") or artifact.get("cpu") or {}
        receiver = artifact.get("receiver") or {}
        data = (capacity.get("firstData") or {}).get("packets")
        service_us = (capacity.get("serviceSignals") or {}).get("durationSumUs")
        rows.append(
            {
                "artifact": str(path),
                "crypto": crypto,
                "repeat": crypto_repeat(path),
                "outputs": config.get("outputs"),
                "offeredPayloadGbps": (capacity.get("offered") or {}).get("payloadGbps"),
                "dataFirst": data,
                "egressThreadUsPerDataFirst": cpu.get("egressThreadUsPerDataFirst"),
                "processUsPerDataFirst": cpu.get("processUsPerDataFirst"),
                "serviceDurationUsPerDataFirst": (
                    service_us / data
                    if finite(service_us) and finite(data) and data > 0
                    else None
                ),
                "receiverCpuSysMs": receiver.get("cpuSysMs"),
                "receiverCpuUserMs": receiver.get("cpuUserMs"),
            }
        )
    return rows


def value_stats(rows, field):
    values = [row.get(field) for row in rows if finite(row.get(field))]
    if not values:
        return {"count": 0, "median": None, "min": None, "max": None}
    return {
        "count": len(values),
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
    }


def summarize_crypto(root):
    rows = load_crypto_rows(root)
    by_mode = {mode: [row for row in rows if row["crypto"] == mode] for mode in ("128", "256")}
    modes = {
        mode: {
            "cells": cells,
            "count": len(cells),
            "egressThreadUsPerDataFirst": value_stats(cells, "egressThreadUsPerDataFirst"),
            "processUsPerDataFirst": value_stats(cells, "processUsPerDataFirst"),
            "serviceDurationUsPerDataFirst": value_stats(
                cells, "serviceDurationUsPerDataFirst"
            ),
            "receiverCpuSysMs": value_stats(cells, "receiverCpuSysMs"),
            "receiverCpuUserMs": value_stats(cells, "receiverCpuUserMs"),
        }
        for mode, cells in by_mode.items()
    }
    paired = []
    indexed = {}
    for mode, cells in by_mode.items():
        for row in cells:
            indexed.setdefault(row.get("repeat"), {})[mode] = row
    for repeat, pair in sorted(indexed.items(), key=lambda item: (item[0] is None, item[0])):
        if "128" not in pair or "256" not in pair:
            continue
        aes128 = pair["128"]
        aes256 = pair["256"]
        delta = {
            "repeat": repeat,
            "egressThreadUsPerDataFirst": aes256["egressThreadUsPerDataFirst"]
            - aes128["egressThreadUsPerDataFirst"],
            "processUsPerDataFirst": aes256["processUsPerDataFirst"]
            - aes128["processUsPerDataFirst"],
            "serviceDurationUsPerDataFirst": aes256[
                "serviceDurationUsPerDataFirst"
            ]
            - aes128["serviceDurationUsPerDataFirst"],
        }
        paired.append(delta)
    delta_stats = {
        field: value_stats(paired, field)
        for field in (
            "egressThreadUsPerDataFirst",
            "processUsPerDataFirst",
            "serviceDurationUsPerDataFirst",
        )
    }
    enough_repeats = len(by_mode["128"]) >= 3 and len(by_mode["256"]) >= 3
    egress_delta = delta_stats["egressThreadUsPerDataFirst"]
    noise_spans_zero = (
        egress_delta["count"] > 0
        and egress_delta["min"] <= 0 <= egress_delta["max"]
    )
    unresolved = not enough_repeats or egress_delta["count"] == 0 or noise_spans_zero
    return {
        "modes": modes,
        "pairedDeltas": paired,
        "pairedDeltaStats": delta_stats,
        "noiseSpansZero": noise_spans_zero,
        "unresolvedOnHost": unresolved,
        "basis": "apparatus-valid bounded cells; use medians/min/max, not one sample",
        "fullMatrix": "not run; expand only if repeated bounded cells show nonlinear behavior",
    }


def summarize(root, target_utilization):
    rows = load_rows(root)
    arms = {}
    for row in rows:
        shards = row.get("shards")
        if not isinstance(shards, int):
            continue
        arms.setdefault(str(shards), []).append(row)

    arm_summaries = {}
    joint_points = []
    required_rows = []
    for key, arm in sorted(arms.items(), key=lambda item: int(item[0])):
        arm.sort(key=lambda row: row.get("outputs") or 0)
        valid = [
            row
            for row in arm
            if row.get("apparatusValid") is True
            and finite(row.get("dataRate"))
            and finite(row.get("egressCoreEquivalents"))
        ]
        fit_rows = [row for row in valid if row.get("classification") != "sender-saturated"]
        fit = fit_affine(
            [(row["dataRate"], row["egressCoreEquivalents"]) for row in fit_rows]
        )
        joint_points.extend(
            (int(key), row["dataRate"], row["egressCoreEquivalents"]) for row in fit_rows
        )
        receiver_capped = any(
            row.get("classification") == "receiver-apparatus-limited" for row in arm
        )
        sender_knee = next(
            (row for row in arm if row.get("classification") == "sender-saturated"),
            None,
        )
        arm_summaries[key] = {
            "rows": arm,
            "validRows": len(valid),
            "fitRows": len(fit_rows),
            "receiverCapped": receiver_capped,
            "senderKnee": sender_knee,
            "cpuDemandFit": fit,
            "medianEgressCoreEquivalents": median_field(
                fit_rows, "egressCoreEquivalents"
            ),
            "medianDataRate": median_field(fit_rows, "dataRate"),
            "medianProcessUsPerDataFirst": median_field(
                fit_rows, "processUsPerDataFirst"
            ),
            "medianShardCpuImbalance": median_field(fit_rows, "shardCpuImbalance"),
        }

    joint_fit = fit_joint(joint_points)
    fixed_per_shard = joint_fit.get("fixedCorePerShard")
    seconds_per_data = joint_fit.get("secondsPerData")
    denominator = (
        target_utilization - fixed_per_shard
        if finite(fixed_per_shard)
        else None
    )
    provisional_required = []
    if finite(seconds_per_data) and finite(denominator) and denominator > 0:
        for row in rows:
            if row.get("apparatusValid") is not True or row.get("classification") == "sender-saturated":
                continue
            if not finite(row.get("dataRate")):
                continue
            required = math.ceil(row["dataRate"] * seconds_per_data / denominator)
            required = max(1, required)
            row["provisionalRequiredShards"] = required
            row["targetShardUtilization"] = target_utilization
            row["fixedOverheadGuard"] = "fixed per-shard CPU is included; retain a tail-latency guard"
            provisional_required.append(required)
            required_rows.append(row)

    shard_comparisons = []
    arm_keys = sorted(arm_summaries, key=int)
    for previous, current in zip(arm_keys, arm_keys[1:]):
        previous_summary = arm_summaries[previous]
        current_summary = arm_summaries[current]
        previous_core = previous_summary["medianEgressCoreEquivalents"]
        current_core = current_summary["medianEgressCoreEquivalents"]
        core_change = None
        if finite(previous_core) and finite(current_core) and previous_core:
            core_change = 1.0 - current_core / previous_core
        shard_comparisons.append(
            {
                "fromShards": int(previous),
                "toShards": int(current),
                "egressCoreDemandReduction": core_change,
                "fromMedianProcessUsPerDataFirst": previous_summary[
                    "medianProcessUsPerDataFirst"
                ],
                "toMedianProcessUsPerDataFirst": current_summary[
                    "medianProcessUsPerDataFirst"
                ],
                "fixedOverheadGuard": "accept an extra shard only when CPU/tail gain pays fixed runtime and wakeup cost",
            }
        )

    return {
        "mode": "wi3.7-capacity-analysis",
        "root": str(root),
        "shardComparisons": shard_comparisons,
        "arms": arm_summaries,
        "jointCpuDemandFit": joint_fit,
        "cryptoIncrement": summarize_crypto(root),
        "portableCoefficient": {
            "provisional": True,
            "requiredShardsUpperBound": max(provisional_required)
            if provisional_required
            else None,
            "targetShardUtilization": target_utilization,
            "fixedCorePerShard": fixed_per_shard,
            "secondsPerData": seconds_per_data,
            "equation": "ceil(DATA_rate * seconds_per_DATA / (target_utilization - fixed_core_per_shard)) + tail guard",
            "productionDefault": "unchanged; defer portable coefficient and default choice to WI10",
            "requiredRows": len(required_rows),
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
