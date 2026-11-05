#!/usr/bin/env python3
"""Compare full reconciliation and direct-file visibility through one MCP session."""

import argparse
import hashlib
import json
import math
import os
import platform
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BINARY = ROOT / "target" / "release" / "aether-midnight"


def distribution(values):
    ordered = sorted(values)
    if not ordered:
        return {"sample_count": 0, "p50_ms": None, "p95_ms": None, "p99_ms": None, "samples_ms": []}

    def percentile(p):
        return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)]

    return {
        "sample_count": len(ordered),
        "p50_ms": percentile(0.50),
        "p95_ms": percentile(0.95),
        "p99_ms": percentile(0.99),
        "samples_ms": ordered,
    }


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def database_bytes(path):
    return sum(
        candidate.stat().st_size
        for candidate in (path, Path(f"{path}-wal"), Path(f"{path}-shm"))
        if candidate.exists()
    )


class McpClient:
    def __init__(self, binary, database):
        self.next_id = 0
        self.process = subprocess.Popen(
            [str(binary), "--db", str(database), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "refresh-file-benchmark", "version": "1"},
            },
        )
        self.notify("notifications/initialized", {})

    def request(self, method, params):
        self.next_id += 1
        message = {
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        }
        started = time.perf_counter()
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        elapsed_ms = (time.perf_counter() - started) * 1000
        if not line:
            raise RuntimeError(f"MCP server exited: {self.process.stderr.read()}")
        response = json.loads(line)
        if response.get("id") != self.next_id or "error" in response:
            raise RuntimeError(f"unexpected {method} response: {response}")
        return elapsed_ms, response

    def notify(self, method, params):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def tool(self, name, arguments):
        elapsed_ms, response = self.request(
            "tools/call", {"name": name, "arguments": arguments}
        )
        result = response["result"]
        if result.get("isError"):
            raise RuntimeError(f"MCP tool {name} failed: {result}")
        text = next(item["text"] for item in result["content"] if "text" in item)
        return elapsed_ms, json.loads(text)

    def close(self):
        self.process.stdin.close()
        try:
            returncode = self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            returncode = self.process.wait(timeout=5)
        if returncode:
            raise RuntimeError(f"MCP process exited {returncode}: {self.process.stderr.read()}")


def write_probe(path, token):
    previous_mtime = path.stat().st_mtime_ns if path.exists() else 0
    path.write_text(
        f"// {token}\npub fn refresh_probe() -> &'static str {{ \"{token}\" }}\n",
        encoding="utf-8",
    )
    now = time.time_ns()
    mtime = max(now, previous_mtime + 1_000_000)
    os.utime(path, ns=(mtime, mtime))


def alphabetic_suffix(value):
    suffix = ""
    for _ in range(3):
        suffix = chr(ord("a") + value % 26) + suffix
        value //= 26
    return suffix


def verify_hit(payload, relative_path, expected_complete):
    results = payload.get("results", [])
    if not results or results[0].get("path") != relative_path:
        raise RuntimeError(f"expected {relative_path} first, got {results[:1]}")
    if results[0].get("freshness") != "fresh":
        raise RuntimeError(f"result is not fresh: {results[0]}")
    if results[0].get("index_complete") is not expected_complete:
        raise RuntimeError(
            f"expected result index_complete={expected_complete}, got {results[0].get('index_complete')}"
        )
    return results[0]


def run(binary, file_count, samples):
    with tempfile.TemporaryDirectory(prefix="midnight-refresh-benchmark-") as temp:
        base = Path(temp)
        repo = base / "repo"
        source_dir = repo / "src"
        source_dir.mkdir(parents=True)
        relative_path = "src/00000.rs"
        target = repo / relative_path
        write_probe(target, "initialvisibilityprobe")
        for index in range(1, file_count):
            (source_dir / f"{index:05}.txt").write_text(
                f"background repository record {index:05} common context material\n",
                encoding="utf-8",
            )

        database = base / "midnight.sqlite3"
        initial = subprocess.run(
            [str(binary), "--db", str(database), "index", "--repo", str(repo)],
            capture_output=True,
            text=True,
            check=False,
        )
        if initial.returncode:
            raise RuntimeError(f"initial full index failed: {initial.stderr[-2000:]}")
        initial_status = json.loads(initial.stdout)
        if not initial_status.get("catalog_complete"):
            raise RuntimeError("initial index did not complete the catalog")
        if initial_status.get("eligible_files") != file_count:
            raise RuntimeError(
                f"expected {file_count} fixture files, indexed {initial_status.get('eligible_files')}"
            )

        client = McpClient(binary, database)
        full_ms = []
        direct_ms = []
        full_bytes = []
        direct_bytes = []
        for sample in range(samples):
            suffix = alphabetic_suffix(sample)
            full_token = f"fullvisibilityprobe{suffix}"
            write_probe(target, full_token)
            started = time.perf_counter()
            _, index_payload = client.tool(
                "index_repository", {"repo_path": str(repo)}
            )
            _, full_search = client.tool(
                "search_code",
                {"repo_path": str(repo), "query": full_token, "limit": 1},
            )
            verify_hit(full_search, relative_path, True)
            if not index_payload.get("catalog_complete"):
                raise RuntimeError("full reconciliation did not complete the catalog")
            full_ms.append((time.perf_counter() - started) * 1000)
            full_bytes.append(database_bytes(database))

            direct_token = f"directvisibilityprobe{suffix}"
            write_probe(target, direct_token)
            started = time.perf_counter()
            _, refresh_payload = client.tool(
                "refresh_indexed_file",
                {"repo_path": str(repo), "path": relative_path},
            )
            _, direct_search = client.tool(
                "search_code",
                {"repo_path": str(repo), "query": direct_token, "limit": 1},
            )
            verify_hit(direct_search, relative_path, False)
            if refresh_payload.get("catalog_complete") is not False:
                raise RuntimeError("direct refresh did not mark catalog completeness partial")
            direct_ms.append((time.perf_counter() - started) * 1000)
            direct_bytes.append(database_bytes(database))

        _, final_index = client.tool("index_repository", {"repo_path": str(repo)})
        if not final_index.get("catalog_complete"):
            raise RuntimeError("final reconciliation did not complete the catalog")
        client.close()
        return {
            "schema_version": 1,
            "created_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "binary": str(binary),
            "binary_sha256": sha256(binary),
            "binary_version": subprocess.run(
                [str(binary), "--version"], capture_output=True, text=True, check=True
            ).stdout.strip(),
            "source_commit": subprocess.run(
                ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip(),
            "host": {
                "os": platform.platform(),
                "architecture": platform.machine(),
            },
            "fixture": {
                "file_count": file_count,
                "background_files": file_count - 1,
                "repo_contains_git_metadata": False,
                "target_path": relative_path,
            },
            "samples": samples,
            "index_complete_after_initial_scan": initial_status.get("catalog_complete"),
            "eligible_files_after_initial_scan": initial_status.get("eligible_files"),
            "full_reconciliation_visibility": {
                **distribution(full_ms),
                "database_bytes_samples": full_bytes,
                "all_searches_fresh": True,
                "catalog_complete": True,
            },
            "direct_file_refresh_visibility": {
                **distribution(direct_ms),
                "database_bytes_samples": direct_bytes,
                "all_searches_fresh": True,
                "catalog_partial_until_reconciliation": True,
            },
            "eligible_files_after_final_reconciliation": final_index.get("eligible_files"),
            "reconciled_catalog_complete_at_end": final_index.get("catalog_complete"),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--files", type=int, default=10_000)
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"binary does not exist: {args.binary}")
    if not 10 <= args.files <= 100_000:
        parser.error("--files must be between 10 and 100000")
    if not 1 <= args.samples <= 100:
        parser.error("--samples must be between 1 and 100")
    result = run(args.binary.resolve(), args.files, args.samples)
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
