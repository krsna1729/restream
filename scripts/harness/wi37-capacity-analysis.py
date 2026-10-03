#!/usr/bin/env python3
"""Summarize provenance-clean WI3.7 egress-duty artifacts.

The input tree is produced by ``wi37-capacity.sh``. A provenance selection is
required; every selected artifact must have a sibling ``contract.json`` that
matches it exactly. Plaintext attempts for one logical cell are retained in
the output and reduced to a median for fitting. A cell that flips between
stable and sender-saturated is marked boundary-unstable and excluded from the
stable service-demand fit.

The fit is measurement aid for WI10/WI8, not a production shard decision:

    egress cores = fixed cores/shard * shard count + seconds/DATA * DATA rate
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import statistics
from pathlib import Path


PROVENANCE_KEYS = (
    "contractVersion",
    "gitSha",
    "benchSha256",
    "restreamSha256",
    "features",
    "buildProvenance",
)
CELL_NUMERIC_FIELDS = (
    "offeredPayloadGbps",
    "dataFirst",
    "windowSecs",
    "serviceDurationSumUs",
    "egressThreadCpuSecs",
    "egressThreadUsPerDataFirst",
    "processUsPerDataFirst",
    "hottestShardCpuUtilization",
    "shardCpuImbalance",
    "dataRate",
    "egressCoreEquivalents",
    "serviceDurationUsPerDataFirst",
    "receiverCpuSysMs",
    "receiverCpuUserMs",
)


def finite(value):
    return isinstance(value, (int, float)) and math.isfinite(value)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def load_provenance(path):
    provenance = read_json(path)
    if not isinstance(provenance, dict):
        raise ValueError(f"invalid provenance selection: {path}")
    if any(key not in provenance for key in PROVENANCE_KEYS):
        raise ValueError(f"provenance selection lacks required fields: {path}")
    if provenance["contractVersion"] != 2:
        raise ValueError("WI3.7 analysis requires contractVersion=2")
    build = provenance["buildProvenance"]
    if (
        not isinstance(build, dict)
        or build.get("gitSha") != provenance["gitSha"]
        or build.get("gitDirty") is not False
        or build.get("features") != provenance["features"]
    ):
        raise ValueError("provenance selection is not a clean feature build")
    return provenance


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
    residuals = [
        cores - predicted for predicted, (_, _, cores) in zip(predictions, points)
    ]
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


def crypto_repeat(path):
    match = re.search(r"repeat-(\d+)", str(path))
    return int(match.group(1)) if match else None


def contract_matches_selection(contract, selection):
    if not isinstance(contract, dict):
        return False
    return all(contract.get(key) == selection.get(key) for key in PROVENANCE_KEYS)


def artifact_matches_contract(artifact, contract):
    config = artifact.get("config") or {}
    checks = {
        "crypto": config.get("crypto"),
        "requestedShards": config.get("requestedShards"),
        "outputs": config.get("outputs"),
        "bitrate": config.get("bitrate"),
        "windowSecs": config.get("windowSecs"),
        "receiverQueueHorizonMs": config.get("receiverQueueHorizonMs"),
        "destBase": config.get("destBase"),
        "netns": config.get("netns"),
        "shardCpus": ",".join(str(value) for value in config.get("shardCpus", [])),
        "controlRestreamCpus": config.get(
            "restreamCpusExplicit", config.get("restreamCpus")
        ),
        "harnessCpus": config.get("harnessCpus"),
        "receiverPeerCpus": config.get("peerCpus"),
    }
    for key, value in checks.items():
        expected = contract.get(key)
        if key == "windowSecs":
            if value is None or expected is None or abs(float(value) - float(expected)) > 1e-9:
                return False
        elif str(value) != str(expected):
            return False
    return contract.get("controlRestreamHarnessShare") is True


def selected_artifact_records(root, selection):
    """Return every artifact with a matching sibling contract; never use mtime."""
    records = []
    for path in sorted(root.rglob("egress-duty.json")):
        contract_path = path.parent / "contract.json"
        artifact = read_json(path)
        contract = read_json(contract_path)
        if not isinstance(artifact, dict) or not contract_matches_selection(contract, selection):
            continue
        if artifact.get("verdict") is None or not isinstance(artifact.get("capacity"), dict):
            continue
        if not artifact_matches_contract(artifact, contract):
            continue
        records.append(
            {
                "path": path,
                "contractPath": contract_path,
                "artifact": artifact,
                "contract": contract,
            }
        )
    return records


def row_from_record(record):
    path = record["path"]
    artifact = record["artifact"]
    config = artifact.get("config") or {}
    capacity = artifact.get("capacity") or {}
    cpu = capacity.get("cpu") or artifact.get("cpu") or {}
    receiver = artifact.get("receiver") or {}
    data = (capacity.get("firstData") or {}).get("packets")
    window = (artifact.get("window") or {}).get("observedSecs")
    egress_cpu_secs = cpu.get("egressThreadCpuSecs")
    service_us = (capacity.get("serviceSignals") or {}).get("durationSumUs")
    row = {
        "artifact": str(path),
        "artifactSha256": sha256_file(path),
        "contract": str(record["contractPath"]),
        "gitSha": record["contract"].get("gitSha"),
        "benchSha256": record["contract"].get("benchSha256"),
        "restreamSha256": record["contract"].get("restreamSha256"),
        "crypto": config.get("crypto", "plain"),
        "repeat": record["contract"].get("repeat", crypto_repeat(path)),
        "shards": config.get("requestedShards"),
        "outputs": config.get("outputs"),
        "classification": capacity.get("classification"),
        "apparatusValid": capacity.get("apparatusValid"),
        "offeredPayloadGbps": (capacity.get("offered") or {}).get("payloadGbps"),
        "dataFirst": data,
        "windowSecs": window,
        "serviceDurationSumUs": service_us,
        "egressThreadCpuSecs": egress_cpu_secs,
        "egressThreadUsPerDataFirst": cpu.get("egressThreadUsPerDataFirst"),
        "processUsPerDataFirst": cpu.get("processUsPerDataFirst"),
        "hottestShardCpuUtilization": cpu.get("hottestShardCpuUtilization"),
        "shardCpuImbalance": cpu.get("shardCpuImbalance"),
        "receiverCpuSysMs": receiver.get("cpuSysMs"),
        "receiverCpuUserMs": receiver.get("cpuUserMs"),
    }
    row["dataRate"] = (
        data / window if finite(data) and finite(window) and window > 0 else None
    )
    row["egressCoreEquivalents"] = (
        egress_cpu_secs / window
        if finite(egress_cpu_secs) and finite(window) and window > 0
        else None
    )
    row["serviceDurationUsPerDataFirst"] = (
        service_us / data if finite(service_us) and finite(data) and data > 0 else None
    )
    return row


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


def collapse_cell(rows, key_fields):
    rows = sorted(rows, key=lambda row: row["artifact"])
    classifications = sorted({row.get("classification") for row in rows})
    boundary_unstable = {
        "stable-unclassified",
        "sender-saturated",
    }.issubset(classifications)
    apparatus_valid = all(row.get("apparatusValid") is True for row in rows)
    classification = "boundary-unstable" if boundary_unstable else classifications[0]
    if len(classifications) > 1 and not boundary_unstable:
        classification = "mixed"
    cell = {
        key: rows[0].get(key) for key in key_fields
    }
    cell.update(
        {
            "artifact": rows[0]["artifact"] if len(rows) == 1 else None,
            "artifacts": [row["artifact"] for row in rows],
            "contracts": sorted({row["contract"] for row in rows}),
            "artifactSha256": rows[0]["artifactSha256"] if len(rows) == 1 else None,
            "attemptCount": len(rows),
            "attempts": rows,
            "classifications": classifications,
            "classification": classification,
            "boundaryUnstable": boundary_unstable,
            "apparatusValid": apparatus_valid,
            "fitEligible": apparatus_valid and classification == "stable-unclassified",
            "attemptStats": {
                field: value_stats(rows, field) for field in CELL_NUMERIC_FIELDS
            },
        }
    )
    for field in CELL_NUMERIC_FIELDS:
        stats = cell["attemptStats"][field]
        cell[field] = stats["median"]
    return cell


def load_plaintext_cells(root, selection):
    rows = [
        row_from_record(record)
        for record in selected_artifact_records(root, selection)
        if (record["artifact"].get("config") or {}).get("crypto", "plain") == "plain"
    ]
    groups = {}
    for row in rows:
        groups.setdefault((row.get("shards"), row.get("outputs")), []).append(row)
    return [collapse_cell(group, ("shards", "outputs")) for _, group in sorted(groups.items())]


def load_crypto_cells(root, selection):
    rows = [
        row_from_record(record)
        for record in selected_artifact_records(root, selection)
        if (record["artifact"].get("config") or {}).get("crypto") in ("128", "256")
        and record["artifact"].get("capacity", {}).get("apparatusValid") is True
    ]
    groups = {}
    for row in rows:
        repeat = row.get("repeat")
        if repeat is None:
            continue
        groups.setdefault((row.get("crypto"), repeat), []).append(row)
    return [collapse_cell(group, ("crypto", "repeat")) for _, group in sorted(groups.items())]


def median_field(rows, field):
    values = [row.get(field) for row in rows if finite(row.get(field))]
    return statistics.median(values) if values else None


def summarize_crypto(root, selection):
    rows = load_crypto_cells(root, selection)
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
    indexed = {mode: {row["repeat"]: row for row in cells} for mode, cells in by_mode.items()}
    paired = []
    for repeat in sorted(set(indexed["128"]) & set(indexed["256"])):
        aes128 = indexed["128"][repeat]
        aes256 = indexed["256"][repeat]
        fields = (
            "egressThreadUsPerDataFirst",
            "processUsPerDataFirst",
            "serviceDurationUsPerDataFirst",
        )
        if not all(finite(aes128.get(field)) and finite(aes256.get(field)) for field in fields):
            continue
        paired.append(
            {
                "repeat": repeat,
                **{
                    field: aes256[field] - aes128[field] for field in fields
                },
            }
        )
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
        "basis": "provenance-clean apparatus-valid repeat cells; use medians/min/max",
        "fullMatrix": "not run; expand only if repeated bounded cells show nonlinear behavior",
    }


def summarize(root, selection_path, target_utilization):
    selection = load_provenance(selection_path)
    rows = load_plaintext_cells(root, selection)
    arms = {}
    for row in rows:
        shards = row.get("shards")
        if isinstance(shards, int):
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
        fit_rows = [row for row in valid if row.get("fitEligible") is True]
        fit = fit_affine(
            [(row["dataRate"], row["egressCoreEquivalents"]) for row in fit_rows]
        )
        joint_points.extend(
            (int(key), row["dataRate"], row["egressCoreEquivalents"])
            for row in fit_rows
        )
        receiver_capped = any(
            row.get("classification") == "receiver-apparatus-limited" for row in arm
        )
        sender_knee = next(
            (
                row
                for row in arm
                if row.get("classification") in ("sender-saturated", "boundary-unstable")
            ),
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
        target_utilization - fixed_per_shard if finite(fixed_per_shard) else None
    )
    provisional_required = []
    if finite(seconds_per_data) and finite(denominator) and denominator > 0:
        for row in rows:
            if not row.get("fitEligible") or not finite(row.get("dataRate")):
                continue
            required = max(
                1, math.ceil(row["dataRate"] * seconds_per_data / denominator)
            )
            row["provisionalRequiredShards"] = required
            row["targetShardUtilization"] = target_utilization
            row["fixedOverheadGuard"] = (
                "fixed per-shard CPU is included; retain a tail-latency guard"
            )
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
        "provenanceSelection": str(selection_path),
        "provenance": selection,
        "shardComparisons": shard_comparisons,
        "arms": arm_summaries,
        "jointCpuDemandFit": joint_fit,
        "cryptoIncrement": summarize_crypto(root, selection),
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
    parser.add_argument("--provenance", type=Path, required=True)
    parser.add_argument("--target-utilization", type=float, default=0.8)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if not 0.0 < args.target_utilization <= 1.0:
        parser.error("--target-utilization must be in (0, 1]")
    try:
        result = summarize(Path(args.root), args.provenance, args.target_utilization)
    except ValueError as error:
        parser.error(str(error))
    text = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.write_text(text)
    print(text, end="")


if __name__ == "__main__":
    main()
