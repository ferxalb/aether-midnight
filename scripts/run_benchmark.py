#!/usr/bin/env python3
"""Build, generate the temporary reference corpus, and emit benchmark JSON."""

import os
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
        sys.stdout.write(result.stdout)
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
