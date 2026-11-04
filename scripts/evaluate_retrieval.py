#!/usr/bin/env python3
"""Evaluate whole-token, decomposed-identifier, and bounded literal-scan retrieval."""

import json
import math
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "benchmark" / "retrieval_fixture"
QUERIES = json.loads((ROOT / "benchmark" / "retrieval_queries.json").read_text())[
    "queries"
]
BINARY = ROOT / "target" / "release" / "aether-midnight"
MAX_SCAN_BYTES = 4 * 1024 * 1024
MAX_SCAN_FILES = 100


def percentile(values, p):
    ordered = sorted(values)
    return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)] if ordered else 0.0


def invoke(binary, env, *args):
    result = subprocess.run(
        [str(binary), *args], cwd=ROOT, env=env, text=True, capture_output=True
    )
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stderr.strip()}")
    return json.loads(result.stdout)


def database_bytes(path):
    return sum(
        candidate.stat().st_size
        for candidate in (path, Path(f"{path}-wal"), Path(f"{path}-shm"))
        if candidate.exists()
    )


def score_results(queries, results, timings, index_bytes, source_bytes, extra=None):
    details = []
    recall_sum = 0.0
    reciprocal_sum = 0.0
    positive_count = 0
    negative_false_positive_queries = []
    false_positives = []
    unsupported = []
    for query in queries:
        paths = results[query["id"]]
        relevant = set(query["relevant_paths"])
        relevant_ranks = [index for index, path in enumerate(paths, 1) if path in relevant]
        recall = len(relevant_ranks) / len(relevant) if relevant else 0.0
        reciprocal = 1 / min(relevant_ranks) if relevant_ranks else 0.0
        if relevant:
            positive_count += 1
            recall_sum += recall
            reciprocal_sum += reciprocal
        elif paths:
            negative_false_positive_queries.append(query["id"])
        false = [path for path in paths if path not in relevant]
        if false and len(false_positives) < 10:
            false_positives.append({"id": query["id"], "query": query["query"], "paths": false})
        if query["kind"] in ("in-token-fragment", "arbitrary-mid-token-fragment") and recall < 1:
            unsupported.append(query["id"])
        details.append(
            {
                "id": query["id"],
                "language": query["language"],
                "kind": query["kind"],
                "query": query["query"],
                "expected_behavior": query["expected_behavior"],
                "relevant_paths": query["relevant_paths"],
                "top5": paths[:5],
                "recall_at_5": recall,
                "reciprocal_rank": reciprocal,
                "elapsed_ms": timings.get(query["id"]),
            }
        )
    count = len(queries)
    elapsed = [value for value in timings.values()]
    result = {
        "query_count": count,
        "positive_query_count": positive_count,
        "negative_query_count": count - positive_count,
        "recall_at_5": recall_sum / max(positive_count, 1),
        "mrr": reciprocal_sum / max(positive_count, 1),
        "warm_search_ms": {
            "p50_ms": percentile(elapsed, 0.50),
            "p95_ms": percentile(elapsed, 0.95),
            "sample_count": len(elapsed),
            "samples_ms": elapsed,
        },
        "index_bytes": index_bytes,
        "index_to_source_ratio": index_bytes / max(source_bytes, 1),
        "false_positive_examples": false_positives,
        "negative_false_positive_queries": negative_false_positive_queries,
        "unsupported_fragment_queries": unsupported,
        "queries": details,
    }
    if extra:
        result.update(extra)
    return result


def bounded_scan(repo, query):
    needles = [term.casefold() for term in query["query"].split() if term]
    used_bytes = 0
    results = []
    files = sorted(path for path in repo.rglob("*") if path.is_file())
    for path in files[:MAX_SCAN_FILES]:
        size = path.stat().st_size
        if used_bytes + size > MAX_SCAN_BYTES:
            break
        used_bytes += size
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        haystack = text.casefold()
        if needles and any(needle in haystack for needle in needles):
            results.append(path.relative_to(repo).as_posix())
    return results, used_bytes


def evaluate_strategy(repo, db_path, decomposition):
    env = os.environ.copy()
    env["MIDNIGHT_IDENTIFIER_DECOMPOSITION"] = "1" if decomposition else "0"
    env.setdefault("MIDNIGHT_INDEX_WORKERS", "1")
    indexed = invoke(
        BINARY,
        env,
        "--db",
        str(db_path),
        "index",
        "--repo",
        str(repo),
        "--verify-hashes",
    )
    if not indexed["complete"]:
        raise RuntimeError("retrieval-suite fixture index was incomplete")
    raw_results = {}
    timings = {}
    for query in QUERIES:
        started = time.perf_counter()
        response = invoke(
            BINARY,
            env,
            "--db",
            str(db_path),
            "search",
            "--repo",
            str(repo),
            "--query",
            query["query"],
            "--limit",
            "5",
        )
        timings[query["id"]] = (time.perf_counter() - started) * 1000
        raw_results[query["id"]] = [result["path"] for result in response["results"]]
    return env, indexed, raw_results, timings


def verify_scope_and_freshness(repo, db_path, base, env):
    path = repo / "rust" / "src" / "lib.rs"
    original = path.read_text(encoding="utf-8")
    fresh_response = invoke(
        BINARY,
        env,
        "--db",
        str(db_path),
        "search",
        "--repo",
        str(repo),
        "--query",
        "refreshToken",
        "--limit",
        "5",
    )
    fresh_observed = any(
        result["path"] == "rust/src/lib.rs" and result["freshness"] == "fresh"
        for result in fresh_response["results"]
    )
    path.write_text(original + "\n// freshness mutation marker\n", encoding="utf-8")
    stale_response = invoke(
        BINARY,
        env,
        "--db",
        str(db_path),
        "search",
        "--repo",
        str(repo),
        "--query",
        "refreshToken",
        "--limit",
        "5",
    )
    stale_observed = any(
        result["path"] == "rust/src/lib.rs" and result["freshness"] == "stale"
        for result in stale_response["results"]
    )
    path.write_text(original, encoding="utf-8")
    invoke(BINARY, env, "--db", str(db_path), "index", "--repo", str(repo))

    other = base / "other-repository"
    (other / "src").mkdir(parents=True, exist_ok=True)
    (other / "src" / "outsider.rs").write_text(
        "pub fn isolationOnlySecretMarker() {}\n", encoding="utf-8"
    )
    invoke(BINARY, env, "--db", str(db_path), "index", "--repo", str(other))
    original_scope = invoke(
        BINARY,
        env,
        "--db",
        str(db_path),
        "search",
        "--repo",
        str(repo),
        "--query",
        "isolationOnlySecretMarker",
        "--limit",
        "5",
    )
    other_scope = invoke(
        BINARY,
        env,
        "--db",
        str(db_path),
        "search",
        "--repo",
        str(other),
        "--query",
        "isolationOnlySecretMarker",
        "--limit",
        "5",
    )
    return {
        "fresh_result_before_mutation": fresh_observed,
        "stale_result_after_source_mutation": stale_observed,
        "repository_scope_isolation": not original_scope["results"]
        and bool(other_scope["results"]),
        "worktree_scope_isolation": "verified by the Rust test repository_search_isolated_by_worktree_and_repo",
    }


def main():
    if not BINARY.is_file():
        raise RuntimeError("build the release binary before running this evaluator")
    with tempfile.TemporaryDirectory(prefix="midnight-retrieval-suite-") as temp:
        base = Path(temp)
        repo = base / "retrieval-repo"
        shutil.copytree(FIXTURE, repo)
        source_bytes = sum(path.stat().st_size for path in repo.rglob("*") if path.is_file())
        strategies = {}
        for name, decomposition in (("unicode61", False), ("identifier_decomposition", True)):
            db_path = base / f"{name}.sqlite3"
            env, indexed, raw_results, timings = evaluate_strategy(repo, db_path, decomposition)
            size = database_bytes(db_path)
            diag = invoke(BINARY, env, "--db", str(db_path), "diagnose")
            strategies[name] = score_results(
                QUERIES,
                raw_results,
                timings,
                size,
                source_bytes,
                {
                    "sqlite_version": diag["sqlite_version"],
                    "worker_count": indexed["worker_count"],
                    "fixture_files": indexed["eligible_files"],
                    "fixture_bytes": indexed["eligible_bytes"],
                },
            )
            strategies[name]["scope_checks"] = verify_scope_and_freshness(
                repo, db_path, base, env
            )

        base_results = strategies["unicode61"]["queries"]
        literal_results = {entry["id"]: list(entry["top5"]) for entry in base_results}
        literal_timings = {entry["id"]: entry["elapsed_ms"] for entry in base_results}
        total_scanned_bytes = 0
        for query in QUERIES:
            if query["kind"] not in (
                "in-token-fragment",
                "arbitrary-mid-token-fragment",
                "normalized-identifier",
            ):
                continue
            started = time.perf_counter()
            found, scanned_bytes = bounded_scan(repo, query)
            elapsed = (time.perf_counter() - started) * 1000
            total_scanned_bytes += scanned_bytes
            current = literal_results[query["id"]]
            literal_results[query["id"]] = list(dict.fromkeys(current + found))[:5]
            literal_timings[query["id"]] = (literal_timings[query["id"]] or 0.0) + elapsed
        strategies["unicode61_with_bounded_literal_scan"] = score_results(
            QUERIES,
            literal_results,
            literal_timings,
            strategies["unicode61"]["index_bytes"],
            source_bytes,
            {
                "fixture_files": 4,
                "fixture_bytes": source_bytes,
                "max_scan_bytes_per_query": MAX_SCAN_BYTES,
                "max_scan_files_per_query": MAX_SCAN_FILES,
                "total_bytes_scanned": total_scanned_bytes,
                "sqlite_version": strategies["unicode61"]["sqlite_version"],
                "note": "retrieval-only comparison; literal scan is not enabled in the shipped search path",
            },
        )
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "scope": "separate 4-language retrieval fixture; does not alter the 10,000-file performance fixture",
                    "query_count": len(QUERIES),
                    "query_language_counts": {
                        language: sum(query["language"] == language for query in QUERIES)
                        for language in ("rust", "python", "typescript", "tsx")
                    },
                    "old_five_case_quality": "reported separately by the primary benchmark",
                    "scope_checks": {
                        "repo_and_worktree_isolation": "covered by the per-strategy repository check and Rust worktree test",
                    },
                    "strategies": strategies,
                },
                indent=2,
            )
        )


if __name__ == "__main__":
    main()
