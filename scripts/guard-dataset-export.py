#!/usr/bin/env python3
"""Export exact-action Guard JSONL records into supervised JSONL samples.

The exporter intentionally correlates only (trace_id, action_id). A legacy
trace-only outcome is accepted only when that trace has exactly one
classification; ambiguous rows are left incomplete instead of being guessed.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


FORBIDDEN_KEYS = {
    "prompt",
    "secret",
    "token",
    "password",
    "credential",
    "reasoning",
    "chain_of_thought",
    "cot",
    "arguments",
}


def contains_forbidden_key(value: Any) -> str | None:
    if isinstance(value, dict):
        for key, child in value.items():
            lowered = str(key).lower()
            if any(part in lowered for part in FORBIDDEN_KEYS):
                return str(key)
            found = contains_forbidden_key(child)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = contains_forbidden_key(child)
            if found:
                return found
    return None


def read_records(path: Path) -> tuple[list[dict[str, Any]], int]:
    records: list[dict[str, Any]] = []
    rejected = 0
    with path.open("r", encoding="utf-8") as source:
        for line_number, line in enumerate(source, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                print(f"skip line {line_number}: invalid JSON ({error.msg})", file=sys.stderr)
                rejected += 1
                continue
            forbidden = contains_forbidden_key(record)
            if forbidden:
                print(
                    f"skip line {line_number}: unsafe field {forbidden!r}",
                    file=sys.stderr,
                )
                rejected += 1
                continue
            if record.get("record_type") in {
                "classification",
                "approval",
                "execution",
                "compensation",
                "outcome",
            }:
                records.append(record)
    return records, rejected


def export(records: list[dict[str, Any]], include_incomplete: bool) -> list[dict[str, Any]]:
    classifications: list[dict[str, Any]] = []
    approvals: dict[tuple[str, str], str] = {}
    executions: dict[tuple[str, str], str] = {}
    compensations: dict[tuple[str, str], str] = {}
    legacy_by_trace: dict[str, list[dict[str, Any]]] = defaultdict(list)
    counts_by_trace: dict[str, int] = defaultdict(int)

    for record in records:
        kind = record.get("record_type")
        if kind == "classification":
            classifications.append(record)
            counts_by_trace[record.get("trace_id", "")] += 1
        elif kind == "approval":
            approvals[(record.get("trace_id", ""), record.get("action_id", ""))] = record.get(
                "decision"
            )
        elif kind == "execution":
            executions[(record.get("trace_id", ""), record.get("action_id", ""))] = record.get(
                "outcome"
            )
        elif kind == "compensation":
            compensations[(record.get("trace_id", ""), record.get("action_id", ""))] = record.get(
                "outcome"
            )
        elif kind == "outcome":
            legacy_by_trace[record.get("trace_id", "")].append(record)

    samples: list[dict[str, Any]] = []
    for classification in classifications:
        trace_id = classification.get("trace_id", "")
        action_id = classification.get("action_id", "")
        key = (trace_id, action_id)
        approval = approvals.get(key)
        execution = executions.get(key)
        compensation = compensations.get(key)
        if counts_by_trace[trace_id] == 1:
            legacy = legacy_by_trace.get(trace_id, [])
            if approval is None:
                approval = next((row.get("human_approval") for row in legacy if row.get("human_approval") is not None), None)
            if execution is None:
                execution = next((row.get("execution_outcome") for row in legacy if row.get("execution_outcome") is not None), None)

        if not include_incomplete and approval is None and execution is None and compensation is None:
            continue
        samples.append(
            {
                "feature_schema_version": classification.get("feature_schema_version", "AgentChainFeatureV1"),
                "trace_id": trace_id,
                "session_id": classification.get("session_id", ""),
                "action_id": action_id,
                "capability_id": classification.get("capability_id", ""),
                "features": classification.get("chain_features", {}),
                "fast_guard": classification.get("fast_guard", {}),
                "chain_guard": classification.get("chain_guard"),
                "classifier_prediction": classification.get("classifier_prediction"),
                "final_guard_decision": classification.get("final_decision", ""),
                "human_approval": approval,
                "execution_outcome": execution,
                "compensation_outcome": compensation,
                "weak_label": classification.get("weak_label", True),
            }
        )
    return samples


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="Guard JSONL input")
    parser.add_argument("output", type=Path, help="Supervised JSONL output")
    parser.add_argument(
        "--include-incomplete",
        action="store_true",
        help="also emit classifications with no observed outcome yet",
    )
    args = parser.parse_args()

    records, rejected = read_records(args.input)
    samples = export(records, args.include_incomplete)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8", newline="\n") as destination:
        for sample in samples:
            destination.write(json.dumps(sample, ensure_ascii=False, sort_keys=True) + "\n")
    print(
        f"exported {len(samples)} samples from {len(records)} records; rejected {rejected}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

