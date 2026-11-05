#!/usr/bin/env python3
"""Summarize `aether-midnight bench-retrieval` JSON (staged vs v0.3.1 vs opt-in recent)."""

import json
import sys


def main(paths):
    """Print one table per bench-retrieval result file."""
    for path in paths:
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
        workload = data["workload"]
        print(
            f"== {workload['records']} records, {workload['namespaces']} namespace(s), "
            f"{workload['samples_per_strategy']} samples/strategy; db {data['database_bytes']} B"
        )
        for entry in data["classes"]:
            cells = []
            for label, stats in entry["strategies"].items():
                cells.append(
                    f"{label}: p50={stats['p50_ms']:.2f} p95={stats['p95_ms']:.2f} "
                    f"p99={stats['p99_ms']:.2f} cpu={stats['cpu_ms_per_query'] or 0:.2f} "
                    f"[{stats['coverage']}]"
                )
            same = entry["top_10_identical_between_strategies"]
            print(f"{entry['query_class']:34s} same_top10={same} | " + " | ".join(cells))


if __name__ == "__main__":
    main(sys.argv[1:])
