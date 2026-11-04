#!/usr/bin/env python3
"""Build, generate the temporary reference corpus, and emit benchmark JSON."""

import os
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="aether-midnight-bench-") as temp:
        corpus = Path(temp) / "corpus"
        generator = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "generate_fixture.py"), "--output", str(corpus)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=True,
        )
        print(generator.stdout.strip(), file=sys.stderr)
        subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)
        binary = ROOT / "target" / "release" / "aether-midnight"
        result = subprocess.run(
            [str(binary), "bench", "--repo", str(corpus), "--manifest", str(ROOT / "benchmark" / "manifest.json"),
             "--samples", os.environ.get("MIDNIGHT_BENCH_SAMPLES", "30"),
             "--index-samples", os.environ.get("MIDNIGHT_INDEX_SAMPLES", "3"),
             "--stress-cycles", os.environ.get("MIDNIGHT_STRESS_CYCLES", "20")],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="")
        if result.returncode:
            if result.stdout:
                print(result.stdout, file=sys.stderr, end="")
            return result.returncode
        comparison = subprocess.run(
            [str(binary), "fts-eval", "--repo", str(corpus), "--manifest", str(ROOT / "benchmark" / "manifest.json"),
             "--samples", os.environ.get("MIDNIGHT_FTS_SAMPLES", "30")],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
        if comparison.returncode:
            if comparison.stderr:
                print(comparison.stderr, file=sys.stderr, end="")
            if comparison.stdout:
                print(comparison.stdout, file=sys.stderr, end="")
            return comparison.returncode
        retrieval = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "evaluate_retrieval.py")],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
        if retrieval.returncode:
            if retrieval.stderr:
                print(retrieval.stderr, file=sys.stderr, end="")
            if retrieval.stdout:
                print(retrieval.stdout, file=sys.stderr, end="")
            return retrieval.returncode
        report = json.loads(result.stdout)
        report["fts5_tokenizer_evaluation"] = json.loads(comparison.stdout)
        report["expanded_retrieval_evaluation"] = json.loads(retrieval.stdout)
        sys.stdout.write(json.dumps(report, indent=2) + "\n")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
