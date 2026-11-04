#!/usr/bin/env python3
"""Build, generate the temporary reference corpus, and emit benchmark JSON."""

import os
import json
import io
import shutil
import subprocess
import sys
import tempfile
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="aether-midnight-bench-") as temp:
        corpus = Path(temp) / "corpus"
        heldout_repo = Path(temp) / "heldout-repository"
        generator = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "generate_fixture.py"), "--output", str(corpus)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=True,
        )
        print(generator.stdout.strip(), file=sys.stderr)
        suite = json.loads((ROOT / "benchmark" / "suites" / "heldout.json").read_text())
        heldout_commit = suite["corpus_commit"]
        archive = subprocess.run(
            ["git", "archive", "--format=tar", heldout_commit], cwd=ROOT,
            check=True, capture_output=True,
        ).stdout
        heldout_repo.mkdir()
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as tar:
            tar.extractall(heldout_repo)
        # Keep the retrieval fixture, while omitting query labels and prior results.
        for relative in (
            "benchmark/suites", "benchmark/baselines", "benchmark/results.json",
            "benchmark/real-repositories.json", "benchmark/incremental-repositories.json",
            "benchmark/retrieval_queries.json",
        ):
            target = heldout_repo / relative
            if target.is_dir():
                shutil.rmtree(target)
            else:
                target.unlink(missing_ok=True)
        subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)
        binary = ROOT / "target" / "release" / "aether-midnight"
        result = subprocess.run(
            [str(binary), "bench", "--repo", str(corpus), "--manifest", str(ROOT / "benchmark" / "manifest.json"),
             "--heldout-repo", str(heldout_repo),
             "--samples", os.environ.get("MIDNIGHT_BENCH_SAMPLES", "100"),
             "--index-samples", os.environ.get("MIDNIGHT_INDEX_SAMPLES", "30"),
             "--stress-cycles", os.environ.get("MIDNIGHT_STRESS_CYCLES", "30")],
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
            [sys.executable, str(ROOT / "scripts" / "evaluate_fts5.py"), "--repo", str(corpus),
             "--samples", os.environ.get("MIDNIGHT_FTS_SAMPLES", os.environ.get("MIDNIGHT_BENCH_SAMPLES", "100"))],
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
        report["pending_verification"] = {name: "pending (not run)" for name in
                                           ["macOS reference host", "Go", "Node.js", "Bun", "Windows", "ARM"]}
        report["run_configuration"] = {
            "command": [str(binary), "bench", "--repo", "<generated fixture>", "--manifest", "benchmark/manifest.json",
                        "--heldout-repo", str(heldout_repo), "--samples", os.environ.get("MIDNIGHT_BENCH_SAMPLES", "100"),
                        "--index-samples", os.environ.get("MIDNIGHT_INDEX_SAMPLES", "30"),
                        "--stress-cycles", os.environ.get("MIDNIGHT_STRESS_CYCLES", "30")],
            "environment_variables": {name: os.environ.get(name) for name in (
                "PATH", "RUSTUP_TOOLCHAIN", "MIDNIGHT_INDEX_WORKERS", "MIDNIGHT_BENCH_BATCH_FILES",
                "MIDNIGHT_BENCH_BATCH_BYTES", "MIDNIGHT_BENCH_SAMPLES", "MIDNIGHT_INDEX_SAMPLES",
                "MIDNIGHT_STRESS_CYCLES", "MIDNIGHT_FTS_SAMPLES",
            )},
            "effective_sample_defaults": {"MIDNIGHT_BENCH_SAMPLES": "100", "MIDNIGHT_INDEX_SAMPLES": "30",
                                          "MIDNIGHT_STRESS_CYCLES": "30", "MIDNIGHT_FTS_SAMPLES": "100"},
            "network_used_for_fixture": False,
            "heldout_repository_commit": heldout_commit,
            "heldout_repository_method": "clean git archive of the frozen suite corpus commit; query suites and prior results excluded",
        }
        sys.stdout.write(json.dumps(report, indent=2) + "\n")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
