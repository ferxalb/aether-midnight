#!/usr/bin/env python3
"""Run one already-built campaign inside a Modal CPU sandbox."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from performance_campaign import run_campaign, worker_list


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("smoke", "standard", "stress"), required=True)
    parser.add_argument("--samples", type=int, required=True)
    parser.add_argument("--workers", required=True)
    parser.add_argument("--update-workers", type=int, required=True)
    parser.add_argument("--output-root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--max-duration-s", type=int, required=True)
    args = parser.parse_args()
    args.repo = None
    args.interval_ms = 50
    args.workers = worker_list(args.workers)
    try:
        run_dir = run_campaign(args)
    except Exception as error:
        print(json.dumps({"worker_error": f"{type(error).__name__}: {error}"}))
        return 1
    result = {
        "manifest": json.loads((run_dir / "manifest.json").read_text(encoding="utf-8")),
        "summary": json.loads((run_dir / "summary.json").read_text(encoding="utf-8")),
    }
    print(json.dumps(result, separators=(",", ":")))
    return 0 if result["manifest"]["status"] == "completed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
