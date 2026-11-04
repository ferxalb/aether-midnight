#!/usr/bin/env python3
"""Compare SQLite FTS5 trigram and unicode61 on the fixed Midnight fixture."""

import argparse
import json
import math
import os
import re
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmark" / "manifest.json"


def percentile(values: list[float], p: float) -> float:
    ordered = sorted(values)
    index = min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)
    return ordered[index]


def terms(query: str) -> list[str]:
    return [term for term in re.split(r"[^\w]+", query, flags=re.UNICODE) if term]


def match_query(query: str, tokenizer: str) -> Optional[str]:
    tokens = terms(query)
    if tokenizer == "trigram":
        tokens = [term for term in tokens if len(term) >= 3]
    if not tokens:
        return None
    return " OR ".join(f'"{token.replace(chr(34), chr(34) * 2)}"' for token in tokens)


def file_bytes(paths: list[Path]) -> int:
    return sum(path.stat().st_size for path in paths)


def database_bytes(path: Path) -> int:
    return sum(
        candidate.stat().st_size
        for candidate in (path, Path(f"{path}-wal"), Path(f"{path}-shm"))
        if candidate.exists()
    )


def build_index(repo: Path, tokenizer: str, definition: str, expected_files: int, expected_bytes: int):
    files = sorted((repo / "src").glob("*.rs"))
    source_bytes = file_bytes(files)
    if len(files) != expected_files or source_bytes != expected_bytes:
        raise ValueError(
            f"fixture mismatch: expected {expected_files} files/{expected_bytes} bytes, "
            f"got {len(files)} files/{source_bytes} bytes"
        )
    handle, name = tempfile.mkstemp(prefix=f"midnight-{tokenizer}-", suffix=".sqlite3")
    os.close(handle)
    path = Path(name)
    started = time.perf_counter()
    connection = sqlite3.connect(path)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute("PRAGMA synchronous=NORMAL")
    connection.execute("PRAGMA temp_store=FILE")
    connection.execute("CREATE TABLE documents(id INTEGER PRIMARY KEY,path TEXT NOT NULL)")
    connection.execute(
        f"CREATE VIRTUAL TABLE code_fts USING fts5(content,content='',contentless_delete=1,tokenize='{definition}')"
    )
    for start in range(0, len(files), 512):
        rows = []
        for document_id, source in enumerate(files[start : start + 512], start + 1):
            text = source.read_text(encoding="utf-8")
            rows.append((document_id, source.relative_to(repo).as_posix(), text))
        with connection:
            connection.executemany(
                "INSERT INTO documents(id,path) VALUES(?1,?2)",
                ((document_id, rel_path) for document_id, rel_path, _ in rows),
            )
            connection.executemany(
                "INSERT INTO code_fts(rowid,content) VALUES(?1,?2)",
                ((document_id, text) for document_id, _, text in rows),
            )
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    build_ms = (time.perf_counter() - started) * 1000
    return connection, path, {
        "tokenizer": tokenizer,
        "sqlite_version": sqlite3.sqlite_version,
        "build_ms": build_ms,
        "index_bytes": database_bytes(path),
        "index_to_source_ratio": database_bytes(path) / max(source_bytes, 1),
        "eligible_files": len(files),
        "eligible_bytes": source_bytes,
    }


def evaluate_tokenizer(connection, tokenizer: str, manifest: dict, samples: int):
    query_results = []
    latency_samples = []
    recall_sum = 0.0
    reciprocal_sum = 0.0
    eligible_queries = [query for query in manifest["queries"] if query["kind"] != "path"]
    for query in eligible_queries:
        expression = match_query(query["query"], tokenizer)
        if expression is None:
            result_paths = []
            samples_ms = []
        else:
            rows = connection.execute(
                "SELECT documents.path FROM code_fts "
                "JOIN documents ON documents.id=code_fts.rowid "
                "WHERE code_fts MATCH ?1 ORDER BY bm25(code_fts),documents.path LIMIT 5",
                (expression,),
            ).fetchall()
            result_paths = [row[0] for row in rows]
            samples_ms = []
            for _ in range(samples):
                started = time.perf_counter()
                connection.execute(
                    "SELECT documents.path FROM code_fts "
                    "JOIN documents ON documents.id=code_fts.rowid "
                    "WHERE code_fts MATCH ?1 ORDER BY bm25(code_fts),documents.path LIMIT 5",
                    (expression,),
                ).fetchall()
                samples_ms.append((time.perf_counter() - started) * 1000)
                latency_samples.append(samples_ms[-1])
        relevant = set(query["relevant_paths"])
        relevant_ranks = [i for i, path in enumerate(result_paths, 1) if path in relevant]
        recall = len(relevant_ranks) / max(len(relevant), 1)
        reciprocal_rank = 1 / min(relevant_ranks) if relevant_ranks else 0.0
        recall_sum += recall
        reciprocal_sum += reciprocal_rank
        query_results.append(
            {
                "id": query["id"],
                "query": query["query"],
                "top5": result_paths,
                "recall_at_5": recall,
                "reciprocal_rank": reciprocal_rank,
                "search_p50_ms": percentile(samples_ms, 0.5) if samples_ms else None,
                "search_p95_ms": percentile(samples_ms, 0.95) if samples_ms else None,
                "sample_count": len(samples_ms),
            }
        )
    count = len(query_results)
    return {
        "content_query_count": count,
        "recall_at_5": recall_sum / max(count, 1),
        "mrr": reciprocal_sum / max(count, 1),
        "warm_search_ms": {
            "p50_ms": percentile(latency_samples, 0.5),
            "p95_ms": percentile(latency_samples, 0.95),
            "sample_count": len(latency_samples),
        },
        "queries": query_results,
    }


def compare(repo: Path, samples: int) -> dict:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    expected_files = manifest["fixture"]["eligible_files"]
    expected_bytes = manifest["fixture"]["eligible_bytes"]
    configurations = [
        ("trigram", "trigram case_sensitive 0"),
        ("unicode61", "unicode61 remove_diacritics 2"),
    ]
    results = {}
    for name, definition in configurations:
        connection, path, index = build_index(
            repo, name, definition, expected_files, expected_bytes
        )
        try:
            results[name] = {**index, **evaluate_tokenizer(connection, name, manifest, samples)}
        finally:
            connection.close()
            path.unlink(missing_ok=True)
            Path(f"{path}-wal").unlink(missing_ok=True)
            Path(f"{path}-shm").unlink(missing_ok=True)

    path_db = sqlite3.connect(":memory:")
    path_db.execute("CREATE TABLE paths(path TEXT PRIMARY KEY)")
    source_paths = sorted((repo / "src").glob("*.rs"))
    path_db.executemany(
        "INSERT INTO paths(path) VALUES(?)",
        ((source.relative_to(repo).as_posix(),) for source in source_paths),
    )
    path_query = next(query["query"] for query in manifest["queries"] if query["kind"] == "path")
    path_result = path_db.execute("SELECT path FROM paths WHERE path=?", (path_query,)).fetchall()
    if path_result != [(path_query,)]:
        raise RuntimeError(f"the path lookup did not return its fixed relevant path: {path_query}")
    path_samples = []
    for _ in range(samples):
        started = time.perf_counter()
        path_db.execute("SELECT path FROM paths WHERE path=?", (path_query,)).fetchall()
        path_samples.append((time.perf_counter() - started) * 1000)
    path_db.close()
    return {
        "schema_version": 1,
        "scope": "fixed local 10,000-file/100-MiB Midnight fixture",
        "sqlite_version": sqlite3.sqlite_version,
        "method": "contentless FTS5 tables; 512-file batches; code queries from benchmark/manifest.json; path query measured in a separate indexed path table",
        "search_samples_per_content_query_per_tokenizer": samples,
        "path_lookup": {
            "query": path_query,
            "p50_ms": percentile(path_samples, 0.5),
            "p95_ms": percentile(path_samples, 0.95),
            "sample_count": len(path_samples),
            "result": path_result[0][0],
        },
        "tokenizers": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, help="existing generated fixture; default generates one temporarily")
    parser.add_argument("--samples", type=int, default=30)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be at least 1")
    if args.repo:
        result = compare(args.repo.resolve(), args.samples)
    else:
        with tempfile.TemporaryDirectory(prefix="aether-midnight-fts5-") as directory:
            repo = Path(directory) / "corpus"
            subprocess.run(
                [sys.executable, str(ROOT / "scripts" / "generate_fixture.py"), "--output", str(repo)],
                cwd=ROOT,
                check=True,
                stdout=subprocess.DEVNULL,
            )
            result = compare(repo, args.samples)
    json.dump(result, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
