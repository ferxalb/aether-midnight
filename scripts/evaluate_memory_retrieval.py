#!/usr/bin/env python3
"""Evaluate frozen, model-free memory retrieval cases through a persistent MCP process."""

import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "benchmark" / "memory_quality_v1.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "aether-midnight"


def distribution(values):
    ordered = sorted(values)
    if not ordered:
        return {
            "sample_count": 0,
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
            "p99_9_ms": None,
        }

    def percentile(p):
        return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)]

    return {
        "sample_count": len(ordered),
        "p50_ms": percentile(0.50),
        "p95_ms": percentile(0.95),
        "p99_ms": percentile(0.99),
        "p99_9_ms": percentile(0.999),
        "samples_ms": values,
    }


def database_bytes(path):
    return sum(
        candidate.stat().st_size
        for candidate in (path, Path(f"{path}-wal"), Path(f"{path}-shm"))
        if candidate.exists()
    )


def process_rss_bytes(pid):
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, check=False
    )
    try:
        return int(result.stdout.strip()) * 1024
    except ValueError:
        return None


def wal_checkpoint_noop(connection):
    """Read WAL progress without checkpointing or changing its policy."""
    try:
        busy, log_frames, checkpointed_frames = connection.execute(
            "PRAGMA wal_checkpoint(NOOP)"
        ).fetchone()
    except sqlite3.DatabaseError:
        return None
    return {
        "busy": busy,
        "log_frames": log_frames,
        "checkpointed_frames": checkpointed_frames,
        "uncheckpointed_frames": (
            max(log_frames - checkpointed_frames, 0)
            if log_frames >= 0 and checkpointed_frames >= 0
            else None
        ),
    }


class McpClient:
    def __init__(self, binary, database):
        self.next_id = 0
        self.request_count = 0
        self.process = subprocess.Popen(
            [str(binary), "--db", str(database), "mcp"],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def request(self, method, params):
        self.next_id += 1
        self.request_count += 1
        request = {
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        }
        wire_request = json.dumps(request, separators=(",", ":")) + "\n"
        started = time.perf_counter()
        self.process.stdin.write(wire_request)
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        elapsed_ms = (time.perf_counter() - started) * 1000
        if not line:
            error = self.process.stderr.read()
            raise RuntimeError(f"MCP server exited during {method}: {error}")
        response = json.loads(line)
        if "error" in response:
            raise RuntimeError(f"MCP {method} error: {response['error']}")
        return elapsed_ms, response, len(wire_request.encode()) + len(line.encode())

    def call(self, name, arguments):
        elapsed_ms, response, wire_bytes = self.request(
            "tools/call", {"name": name, "arguments": arguments}
        )
        result = response["result"]
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise RuntimeError(f"MCP {name} failed: {text}")
        return elapsed_ms, json.loads(text), wire_bytes, len(text.encode())

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.write('{"jsonrpc":"2.0","method":"exit"}\n')
            self.process.stdin.flush()
            self.process.stdin.close()
        usage = None
        if self.process.returncode is None and hasattr(os, "wait4"):
            _, status, usage = os.wait4(self.process.pid, 0)
            self.process.returncode = os.waitstatus_to_exitcode(status)
        else:
            self.process.wait(timeout=10)
        return_code = self.process.returncode
        if return_code:
            error = self.process.stderr.read()
            raise RuntimeError(f"MCP server returned {return_code}: {error}")
        if usage is None:
            return {"cpu_user_seconds": None, "cpu_system_seconds": None, "cpu_total_seconds": None}
        return {
            "cpu_user_seconds": usage.ru_utime,
            "cpu_system_seconds": usage.ru_stime,
            "cpu_total_seconds": usage.ru_utime + usage.ru_stime,
        }


def fixture_records(fixture):
    records = list(fixture["records"])
    background = fixture["background_collection"]
    records.extend(
        {
            "key": f"background-{index:03d}",
            "scope": background["scope"],
            "kind": "source",
            "content": background["template"].format(index=index),
            "source_uri": f"benchmark://background/{index:03d}",
        }
        for index in range(background["count"])
    )
    return records


def valid_at(memory, at):
    if at is None:
        return True
    return (memory["valid_from"] is None or memory["valid_from"] <= at) and (
        memory["valid_to"] is None or memory["valid_to"] > at
    )


def evaluate_query(query, samples, id_to_key):
    relevant = set(query["relevant"])
    results = samples[-1]["results"]
    actual = [id_to_key.get(item["memory"]["id"], f"unknown:{item['memory']['id']}") for item in results]
    relevant_ranks = [index for index, key in enumerate(actual, start=1) if key in relevant]
    first_relevant_rank = min(relevant_ranks) if relevant_ranks else None
    recall = {}
    for cutoff in (1, 5, 10):
        recall[f"recall_at_{cutoff}"] = (
            len(set(actual[:cutoff]) & relevant) / len(relevant) if relevant else None
        )
    precision_at_5 = len(set(actual[:5]) & relevant) / 5
    dcg = sum(1 / math.log2(rank + 1) for rank in relevant_ranks if rank <= 10)
    ideal_count = min(10, len(relevant))
    ideal_dcg = sum(1 / math.log2(rank + 1) for rank in range(1, ideal_count + 1))
    n_dcg = dcg / ideal_dcg if ideal_dcg else None
    invalid_temporal = [
        id_to_key.get(item["memory"]["id"], f"unknown:{item['memory']['id']}")
        for item in results
        if not valid_at(item["memory"], query.get("at"))
    ]
    temporal_category = query["category"] in {
        "temporal_history",
        "conflicting_and_superseded_facts",
        "long_supersession_chain",
        "temporal_validity",
    }
    temporal_expected = bool(relevant) and relevant <= set(actual[:10]) if relevant else not actual
    content_bytes = sum(len(item["memory"]["content"].encode()) for item in results)
    return {
        "id": query["id"],
        "split": query["split"],
        "category": query["category"],
        "scope": query["scope"],
        "query": query["query"],
        "at": query.get("at"),
        "relevant": sorted(relevant),
        "top10": actual[:10],
        "first_relevant_rank": first_relevant_rank,
        "found_relevant_count_at_10": len(set(actual[:10]) & relevant),
        "returned_count": len(results),
        **recall,
        "precision_at_5": precision_at_5,
        "reciprocal_rank": 1 / first_relevant_rank if first_relevant_rank else 0.0,
        "ndcg_at_10": n_dcg,
        "abstained": len(results) == 0,
        "false_positive": not relevant and bool(results),
        "invalid_temporal_results": invalid_temporal,
        "temporal_correct_at_10": (
            temporal_expected and not invalid_temporal if temporal_category else None
        ),
        "namespace_leaks": [
            item["memory"]["scope"]
            for item in results
            if item["memory"]["scope"] != query["scope"]
        ],
        "latency": distribution([sample["elapsed_ms"] for sample in samples]),
        "response_bytes_p50": sorted(sample["response_bytes"] for sample in samples)[
            (len(samples) - 1) // 2
        ],
        "estimated_content_tokens": math.ceil(content_bytes / 4),
        "evidence_complete_at_10": bool(relevant) and relevant <= set(actual[:10]),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--fixture", type=Path, default=FIXTURE)
    parser.add_argument("--source-commit")
    parser.add_argument("--split", choices=("dev", "heldout", "all"), default="dev")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be at least 1")
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary does not exist: {binary}; run cargo build --release --locked first")

    fixture_path = args.fixture.resolve()
    fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
    queries = [
        query
        for query in fixture["queries"]
        if args.split == "all" or query["split"] == args.split
    ]
    db_dir = Path(tempfile.mkdtemp(prefix="midnight-memory-quality-"))
    database = db_dir / "memory.sqlite3"
    client_started = time.perf_counter()
    client = McpClient(binary, database)
    initialize_request_ms, _, initialize_wire_bytes = client.request(
        "initialize",
        {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "memory-quality-evaluator", "version": "1"},
        },
    )
    startup_initialize_ms = (time.perf_counter() - client_started) * 1000
    first_tool_ms, _, _, _ = client.call(
        "list_memories", {"scope": "user:maya", "limit": 10}
    )
    wal_probe = sqlite3.connect(database, isolation_level=None)

    key_to_id = {}
    id_to_key = {}
    remember_ms = []
    remember_wal_samples = []
    prior_checkpointed_frames = None
    for record in fixture_records(fixture):
        arguments = {
            key: record[key]
            for key in (
                "scope",
                "kind",
                "content",
                "source_uri",
                "observed_at",
                "valid_from",
                "valid_to",
            )
            if key in record
        }
        if "source_key" in record:
            arguments["source_id"] = key_to_id[record["source_key"]]
        if "supersedes_key" in record:
            arguments["supersedes_id"] = key_to_id[record["supersedes_key"]]
        elapsed, memory, _, _ = client.call("remember", arguments)
        remember_ms.append(elapsed)
        wal_status = wal_checkpoint_noop(wal_probe)
        checkpoint_progress = False
        if wal_status is not None:
            checkpointed_frames = wal_status["checkpointed_frames"]
            checkpoint_progress = (
                prior_checkpointed_frames is not None
                and checkpointed_frames > prior_checkpointed_frames
            )
            prior_checkpointed_frames = checkpointed_frames
        remember_wal_samples.append(
            {
                "sample_index": len(remember_ms),
                "elapsed_ms": elapsed,
                "wal_status": wal_status,
                "checkpoint_progress_observed": checkpoint_progress,
            }
        )
        key_to_id[record["key"]] = memory["id"]
        id_to_key[memory["id"]] = record["key"]

    wal_autocheckpoint_pages = wal_probe.execute("PRAGMA wal_autocheckpoint").fetchone()[0]
    wal_probe.close()
    remember_outliers = [sample for sample in remember_wal_samples if sample["elapsed_ms"] >= 4]
    checkpoint_progress_latencies = [
        sample["elapsed_ms"]
        for sample in remember_wal_samples
        if sample["checkpoint_progress_observed"]
    ]
    stable_checkpoint_latencies = [
        sample["elapsed_ms"]
        for sample in remember_wal_samples
        if sample["wal_status"] is not None and not sample["checkpoint_progress_observed"]
    ]
    wal_diagnostics = {
        "probe": "PRAGMA wal_checkpoint(NOOP), sampled after each completed remember call; it does not checkpoint",
        "configured_autocheckpoint_pages": wal_autocheckpoint_pages,
        "status_available": any(sample["wal_status"] is not None for sample in remember_wal_samples),
        "checkpoint_progress_sample_count": len(checkpoint_progress_latencies),
        "latency_with_checkpoint_progress": distribution(checkpoint_progress_latencies),
        "latency_without_observed_checkpoint_progress": distribution(stable_checkpoint_latencies),
        "remember_calls_at_least_4ms": len(remember_outliers),
        "outlier_samples": remember_outliers,
        "raw_samples": remember_wal_samples,
    }

    forget_ms = []
    for key in fixture["forget_keys"]:
        elapsed, forgotten, _, _ = client.call(
            "forget", {"scope": next(r["scope"] for r in fixture["records"] if r["key"] == key), "id": key_to_id[key]}
        )
        if not forgotten["forgotten"]:
            raise RuntimeError(f"fixture forget did not suppress {key}")
        forget_ms.append(elapsed)

    quality_rows = []
    recall_ms = []
    list_ms = []
    response_bytes = []
    for query in queries:
        samples = []
        for _ in range(args.repeats):
            elapsed, results, wire_bytes, response_size = client.call(
                "recall",
                {
                    "scope": query["scope"],
                    "query": query["query"],
                    "at": query.get("at"),
                    "limit": 10,
                },
            )
            recall_ms.append(elapsed)
            response_bytes.append(response_size)
            samples.append(
                {
                    "elapsed_ms": elapsed,
                    "results": results,
                    "wire_bytes": wire_bytes,
                    "response_bytes": response_size,
                }
            )
        quality_rows.append(evaluate_query(query, samples, id_to_key))

    for _ in range(30):
        elapsed, _, _, _ = client.call("list_memories", {"scope": "user:maya", "limit": 100})
        list_ms.append(elapsed)

    delete_ids = []
    for index in range(30):
        _, memory, _, _ = client.call(
            "remember",
            {
                "scope": "user:maya",
                "kind": "source",
                "content": f"temporary deletion profile marker {index}",
                "source_uri": "benchmark://temporary-delete",
            },
        )
        delete_ids.append(memory["id"])
    for memory_id in delete_ids:
        elapsed, result, _, _ = client.call("forget", {"scope": "user:maya", "id": memory_id})
        if not result["forgotten"]:
            raise RuntimeError(f"temporary memory {memory_id} was not deleted")
        forget_ms.append(elapsed)

    rss_before_close = process_rss_bytes(client.process.pid)
    bytes_before_close = database_bytes(database)
    cpu_usage = client.close()
    bytes_after_close = database_bytes(database)
    python_sqlite_version = sqlite3.connect(database).execute("select sqlite_version()").fetchone()[0]
    diagnostics = subprocess.run(
        [str(binary), "--db", str(database), "diagnose"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    engine_diagnostics = json.loads(diagnostics.stdout)
    if args.source_commit:
        commit = args.source_commit
    else:
        try:
            commit = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
            ).strip()
        except subprocess.CalledProcessError:
            commit = None

    positive_rows = [row for row in quality_rows if row["relevant"]]
    negative_rows = [row for row in quality_rows if not row["relevant"]]
    temporal_rows = [row for row in quality_rows if row["temporal_correct_at_10"] is not None]
    total_relevant = sum(len(row["relevant"]) for row in positive_rows)
    found_at_10 = sum(row["found_relevant_count_at_10"] for row in positive_rows)
    temporal_result_count = sum(row["returned_count"] for row in temporal_rows)
    temporal_invalid_count = sum(len(row["invalid_temporal_results"]) for row in temporal_rows)
    try:
        fixture_name = fixture_path.relative_to(ROOT).as_posix()
    except ValueError:
        fixture_name = str(fixture_path)
    results = {
        "schema_version": 1,
        "baseline_commit": commit,
        "source_commit": commit,
        "suite": args.split,
        "fixture": fixture_name,
        "fixture_frozen_at": fixture["frozen_at"],
        "query_count": len(queries),
        "memory_count": len(key_to_id),
        "background_record_count": fixture["background_collection"]["count"],
        "environment": {
            "platform": platform.platform(),
            "python": platform.python_version(),
            "engine_sqlite_version": engine_diagnostics["sqlite_version"],
            "python_sqlite_version_for_artifact_read": python_sqlite_version,
            "binary": str(binary),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        },
        "persistent_mcp": {
            "startup_through_initialize_ms": startup_initialize_ms,
            "initialize_request_ms": initialize_request_ms,
            "initialize_wire_bytes": initialize_wire_bytes,
            "first_tool_list_memories_ms": first_tool_ms,
            "individual_remember": distribution(remember_ms),
            "individual_remember_wal_diagnostics": wal_diagnostics,
            "warm_recall": distribution(recall_ms),
            "list_memories": distribution(list_ms),
            "forget": distribution(forget_ms),
            "response_bytes": distribution(response_bytes),
            "rss_before_close_bytes": rss_before_close,
            "mcp_request_count": client.request_count,
            "cpu_usage": cpu_usage,
            "cpu_seconds_per_request": (
                cpu_usage["cpu_total_seconds"] / client.request_count
                if cpu_usage["cpu_total_seconds"] is not None and client.request_count
                else None
            ),
            "db_wal_shm_bytes_before_close": bytes_before_close,
            "db_wal_shm_bytes_after_close": bytes_after_close,
        },
        "retrieval_quality": {
            "positive_query_count": len(positive_rows),
            "negative_query_count": len(negative_rows),
            "recall_at_1": sum(row["recall_at_1"] for row in positive_rows) / max(len(positive_rows), 1),
            "recall_at_5": sum(row["recall_at_5"] for row in positive_rows) / max(len(positive_rows), 1),
            "recall_at_10": sum(row["recall_at_10"] for row in positive_rows) / max(len(positive_rows), 1),
            "precision_at_5": sum(row["precision_at_5"] for row in positive_rows) / max(len(positive_rows), 1),
            "mrr": sum(row["reciprocal_rank"] for row in positive_rows) / max(len(positive_rows), 1),
            "ndcg_at_10": sum(row["ndcg_at_10"] for row in positive_rows) / max(len(positive_rows), 1),
            "evidence_completeness_at_10": found_at_10 / max(total_relevant, 1),
            "false_positive_queries": [row["id"] for row in negative_rows if row["false_positive"]],
            "false_positive_rate": sum(row["false_positive"] for row in negative_rows) / max(len(negative_rows), 1),
            "temporal_invalid_result_count": sum(len(row["invalid_temporal_results"]) for row in quality_rows),
            "temporal_query_count": len(temporal_rows),
            "temporal_accuracy_at_10": sum(row["temporal_correct_at_10"] for row in temporal_rows) / max(len(temporal_rows), 1),
            "stale_fact_retrieval_count": temporal_invalid_count,
            "stale_fact_retrieval_rate": temporal_invalid_count / max(temporal_result_count, 1),
            "namespace_leak_count": sum(len(row["namespace_leaks"]) for row in quality_rows),
            "query_latency": distribution([row["latency"]["p50_ms"] for row in quality_rows]),
            "estimated_content_tokens_average": sum(row["estimated_content_tokens"] for row in quality_rows) / max(len(quality_rows), 1),
            "context_response_bytes_average": sum(row["response_bytes_p50"] for row in quality_rows) / max(len(quality_rows), 1),
            "queries": quality_rows,
        },
    }
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(results, indent=2, ensure_ascii=False))
    shutil.rmtree(db_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
