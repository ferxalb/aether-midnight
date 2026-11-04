#!/usr/bin/env python3
"""Run FTS5 comparisons through Midnight's bundled rusqlite SQLite runtime."""

import argparse
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "release" / "aether-midnight"
MANIFEST = ROOT / "benchmark" / "manifest.json"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=30)
    args = parser.parse_args()
    if not BINARY.is_file():
        parser.error("build the release binary before running the tokenizer comparison")
    result = subprocess.run(
        [
            str(BINARY),
            "fts-eval",
            "--repo",
            str(args.repo),
            "--manifest",
            str(MANIFEST),
            "--samples",
            str(args.samples),
        ],
        cwd=ROOT,
        env=os.environ.copy(),
        text=True,
    )
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
