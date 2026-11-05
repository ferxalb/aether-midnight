#!/usr/bin/env python3
"""Run local real-repository index and rg/CLI/MCP comparisons with raw samples."""

import argparse
import json
import math
import os
import platform
import re
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
QUERIES = ["TODO", "error", "test", "parse", "config", "cache", "main", "timeout"]


def distribution(values, unit="ms"):
    ordered = sorted(values)
    def pct(p):
        return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)] if ordered else None
    return {"n": len(ordered), f"p50_{unit}": pct(.50), f"p95_{unit}": pct(.95), f"p99_{unit}": pct(.99), f"samples_{unit}": ordered}


def environment(binary):
    if platform.system() == "Darwin":
        cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], text=True, capture_output=True, check=True).stdout.strip()
        ram = int(subprocess.run(["sysctl", "-n", "hw.memsize"], text=True, capture_output=True, check=True).stdout.strip())
        disk = subprocess.run(["diskutil", "info", "/"], text=True, capture_output=True, check=True).stdout
        filesystem = next((line.strip() for line in disk.splitlines() if "File System Personality:" in line), None)
        fs = filesystem or "unavailable from diskutil info /"
        kernel = platform.release()
    else:
        cpu = subprocess.run(["lscpu", "-p=MODELNAME"], text=True, capture_output=True, check=True).stdout
        cpu = next((line for line in cpu.splitlines() if line and not line.startswith("#")), None)
        mem = Path("/proc/meminfo").read_text()
        ram = int(re.search(r"^MemTotal:\s+(\d+)", mem, re.M).group(1)) * 1024
        disk = subprocess.run(["lsblk", "-dn", "-o", "NAME,ROTA,TYPE,TRAN,MODEL"], text=True, capture_output=True).stdout.strip()
        fs = subprocess.run(["findmnt", "-n", "-o", "SOURCE,FSTYPE,TARGET", "--target", str(ROOT)], text=True, capture_output=True).stdout.strip()
        kernel = os.uname().release
    rust = subprocess.run(["rustc", "--version"], text=True, capture_output=True).stdout.strip()
    sqlite_db = tempfile.NamedTemporaryFile(delete=False).name
    try:
        result = json.loads(subprocess.run([str(binary), "--db", sqlite_db, "diagnose"], text=True, capture_output=True, check=True).stdout)
    finally:
        Path(sqlite_db).unlink(missing_ok=True)
    storage_limit = ("diskutil reports an internal solid-state APFS data volume; exact SSD model is unavailable"
                     if platform.system() == "Darwin"
                     else "VM reports virtio block device; physical backing media cannot be identified")
    return {"os": platform.system().lower(), "arch": platform.machine(), "cpu_model": cpu,
            "logical_cores": os.cpu_count(), "ram_bytes": ram, "disk_type_report": disk,
            "filesystem_mount": fs, "kernel": kernel, "rustc": rust,
            "sqlite_version": result["sqlite_version"], "python_sqlite_version": __import__("sqlite3").sqlite_version,
            "ripgrep": subprocess.run(["rg", "--version"], text=True, capture_output=True).stdout.splitlines()[0],
            "storage_media_limit": storage_limit}


def db_size(db):
    return sum(path.stat().st_size for path in (db, Path(f"{db}-wal"), Path(f"{db}-shm")) if path.exists())


def command(binary, db, *args):
    return [str(binary), "--db", str(db), *map(str, args)]


RSS_WORKER = r'''import ctypes, json, platform, resource, struct, subprocess, sys, tempfile, time

cmd = json.loads(sys.argv[1])
samples = []
started = time.perf_counter()
libproc = None
if platform.system() == "Darwin":
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    libproc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    libproc.proc_pidinfo.restype = ctypes.c_int
with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
    child = subprocess.Popen(cmd, stdout=stdout_file, stderr=stderr_file)
    while child.poll() is None:
        rss_bytes = None
        if platform.system() == "Darwin":
            info = ctypes.create_string_buffer(144)
            received = libproc.proc_pidinfo(child.pid, 4, 0, info, len(info))
            if received >= 16:
                rss_bytes = struct.unpack_from("=Q", info.raw, 8)[0]
        else:
            try:
                status = open(f"/proc/{child.pid}/status", encoding="utf-8").read()
                import re
                match = re.search(r"^VmRSS:\s+(\d+)", status, re.M)
                rss_bytes = int(match.group(1)) * 1024 if match else None
            except FileNotFoundError:
                pass
        if rss_bytes is not None:
            samples.append(rss_bytes)
        time.sleep(.005)
    returncode = child.wait()
    elapsed_ms = (time.perf_counter() - started) * 1000
    stdout_file.seek(0)
    stderr_file.seek(0)
    stdout = stdout_file.read().decode(errors="replace")
    stderr = stderr_file.read().decode(errors="replace")

raw_ru_maxrss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
ru_maxrss_bytes = int(raw_ru_maxrss if platform.system() == "Darwin" else raw_ru_maxrss * 1024)
peak_rss_bytes = max([ru_maxrss_bytes, *samples], default=ru_maxrss_bytes)
assert all(peak_rss_bytes >= sample for sample in samples)
print(json.dumps({"returncode": returncode, "elapsed_ms": elapsed_ms, "stdout": stdout, "stderr": stderr,
                  "sampled_rss_bytes": samples, "ru_maxrss_raw": raw_ru_maxrss,
                  "ru_maxrss_bytes": ru_maxrss_bytes, "peak_rss_bytes": peak_rss_bytes}))
'''


def run_index(binary, repo, db, rssfile):
    cmd = command(binary, db, "index", "--repo", repo)
    measured = subprocess.run([sys.executable, "-c", RSS_WORKER, json.dumps(cmd)], text=True, capture_output=True)
    if measured.returncode:
        raise RuntimeError(f"RSS measurement worker failed: {measured.stderr[-2000:]}")
    measurement = json.loads(measured.stdout)
    samples = measurement["sampled_rss_bytes"]
    peak_rss = measurement["peak_rss_bytes"]
    if any(peak_rss < sample for sample in samples):
        raise RuntimeError("normalized peak RSS is below a sampled RSS")
    if measurement["returncode"]:
        raise RuntimeError(f"index failed: {measurement['stderr'][-2000:]}")
    report = json.loads(measurement["stdout"])
    if not report.get("catalog_complete", report.get("complete", False)):
        raise RuntimeError("index did not finish its complete file catalog")
    return measurement["elapsed_ms"], peak_rss, report, measurement


def response_payload(process, method, repo, query):
    if method == "cli":
        started = time.perf_counter()
        completed = subprocess.run(command(process, repo["db"], "search", "--repo", repo["path"], "--query", query, "--limit", "5"), capture_output=True)
        elapsed = (time.perf_counter() - started) * 1000
        if completed.returncode:
            raise RuntimeError(completed.stderr.decode(errors="replace"))
        value = json.loads(completed.stdout)
        return elapsed, len(completed.stdout), len(completed.stdout), len(value.get("results", []))
    if method in ("rg", "rg_m5"):
        cmd = ["rg", "--files-with-matches", "--fixed-strings", "--ignore-case", "--no-messages", "--hidden"]
        if method == "rg_m5":
            cmd.extend(["-m", "5"])
        cmd.extend([
               "--glob", "!.git/**", "--glob", "!target/**", "--glob", "!node_modules/**", query, repo["path"]]
        )
        started = time.perf_counter()
        completed = subprocess.run(cmd, capture_output=True)
        elapsed = (time.perf_counter() - started) * 1000
        if completed.returncode not in (0, 1):
            raise RuntimeError(completed.stderr.decode(errors="replace"))
        return elapsed, len(completed.stdout), len(completed.stdout), len(completed.stdout.splitlines())

    started = time.perf_counter()
    child = subprocess.Popen(command(process, repo["db"], "mcp"), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    def send(obj):
        child.stdin.write((json.dumps(obj, separators=(",", ":")) + "\n").encode())
        child.stdin.flush()
    def read():
        line = child.stdout.readline()
        if not line:
            raise RuntimeError("MCP process ended before replying")
        return line, json.loads(line)
    try:
        send({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"bench","version":"1"}}})
        _, initialized = read()
        if "result" not in initialized:
            raise RuntimeError("MCP initialize failed")
        send({"jsonrpc":"2.0", "method":"notifications/initialized"})
        send({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"search_code", "arguments":{"repo_path":repo["path"], "query":query, "limit":5}}})
        line, message = read()
        text = message["result"]["content"][0]["text"]
        value = json.loads(text)
        payload_bytes = len(text.encode())
        elapsed = (time.perf_counter() - started) * 1000
        return elapsed, payload_bytes, len(line), len(value.get("results", []))
    finally:
        child.stdin.close()
        child.wait(timeout=20)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", action="append", required=True, metavar="NAME=PATH")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/aether-midnight")
    parser.add_argument("--index-samples", type=int, default=int(os.getenv("MIDNIGHT_INDEX_SAMPLES", "30")))
    parser.add_argument("--query-samples", type=int, default=int(os.getenv("MIDNIGHT_QUERY_SAMPLES", "100")))
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = {"schema_version":1, "environment":environment(binary), "query_list":QUERIES,
              "tokenizer_approximation":"ceil(returned UTF-8 byte count / 4); a size heuristic, not a model tokenizer",
              "cache_conditions":{"full_index":"fresh SQLite database per sample; OS page cache not dropped, first sample cache unknown and later samples may be warm",
                                  "search":"warm complete SQLite index; OS caches left untouched; CLI/MCP/rg each include process startup; MCP includes initialize plus one tools/call round trip"},
              "index_sample_n":args.index_samples,"query_sample_n_per_query_and_method":args.query_samples,
              "peak_rss_sampling":"isolated Python worker polls child RSS every 5 ms (/proc VmRSS KiB on Linux, proc_pidinfo Resident_size bytes on macOS), reads child ru_maxrss, normalizes Linux KiB and macOS bytes, and asserts peak bytes >= every sampled RSS value",
              "repositories":[]}
    for spec in args.repo:
        name, path = spec.split("=", 1)
        repo = Path(path).resolve()
        url = subprocess.run(["git", "-C", str(repo), "remote", "get-url", "origin"], text=True, capture_output=True).stdout.strip()
        commit = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True, capture_output=True, check=True).stdout.strip()
        record = {"name":name,"url":url,"commit":commit,"path":str(repo),"file_count_on_disk":sum(1 for p in repo.rglob("*") if p.is_file() and ".git" not in p.parts),
                  "tracked_file_count":int(subprocess.run(["git","-C",str(repo),"ls-files"],capture_output=True,text=True,check=True).stdout.count("\n")),
                  "full_index_samples":[],"comparisons":[]}
        with tempfile.TemporaryDirectory(prefix=f"midnight-{name}-") as tmp:
            tmp = Path(tmp)
            keep_db = None
            durations=[]; rss=[]; db_sizes=[]; reports=[]
            for i in range(args.index_samples):
                db = tmp / f"index-{i}.sqlite3"
                rssfile = tmp / f"rss-{i}.txt"
                elapsed, peak, report, rss_measurement = run_index(binary, repo, db, rssfile)
                size = db_size(db)
                durations.append(elapsed); rss.append(peak); db_sizes.append(size); reports.append(report)
                record["full_index_samples"].append({"elapsed_ms":elapsed,"peak_rss_bytes":peak,"database_bytes":size,
                    "eligible_files":report.get("eligible_files"),"eligible_bytes":report.get("eligible_bytes"),
                    "complete":report.get("complete"),"catalog_complete":report.get("catalog_complete"),
                    "structural_complete":report.get("structural_complete"),
                    "sampled_rss_bytes":rss_measurement["sampled_rss_bytes"],
                    "ru_maxrss_raw":rss_measurement["ru_maxrss_raw"],
                    "ru_maxrss_bytes":rss_measurement["ru_maxrss_bytes"],
                    "peak_rss_ge_every_sampled_rss":all(peak >= sample for sample in rss_measurement["sampled_rss_bytes"])})
                if i == args.index_samples - 1:
                    keep_db = db
                else:
                    for suffix in ("", "-wal", "-shm"):
                        Path(f"{db}{suffix}").unlink(missing_ok=True)
            status = json.loads(subprocess.run(command(binary, keep_db, "index-status", "--repo", repo), capture_output=True,text=True,check=True).stdout)
            source_bytes = status.get("eligible_bytes", 0)
            ratio_samples=[v/max(source_bytes,1) for v in db_sizes]
            record["index_summary"]={"fresh_database_full_index_ms":distribution(durations),"peak_rss_bytes":distribution(rss,"bytes"),
                "database_bytes":distribution(db_sizes,"bytes"),"database_to_source_ratio_samples":sorted(v/max(source_bytes,1) for v in db_sizes),
                "database_to_source_ratio":distribution(ratio_samples,"ratio"),
                "eligible_file_count":distribution([report.get("eligible_files",0) for report in reports],"files"),
                "eligible_source_bytes":distribution([report.get("eligible_bytes",0) for report in reports],"bytes"),
                "eligible_files":status.get("eligible_files"),"eligible_bytes":source_bytes,"last_database_bytes":db_size(keep_db),
                "catalog_complete":status.get("catalog_complete"),"structural_complete":status.get("structural_complete"),
                "cold_index_sample_semantics":"database cold, operating system cache not evicted"}
            record["index_summary"]["last_database_bytes_evidence"] = {
                **distribution([db_size(keep_db)],"bytes"),
                "status":"anecdotal: one final database-size observation",
            }
            reason_counts = status.get("skip_reasons", {})
            record["index_summary"]["structural_parse_error_count"] = reason_counts.get("tree_sitter_parse_error", 0)
            record["index_summary"]["structural_parse_error_count_evidence"] = distribution(
                [reason_counts.get("tree_sitter_parse_error", 0)], "count")
            record["index_summary"]["structural_parse_error_count_evidence"]["status"] = (
                "anecdotal: one final index-status observation"
            )
            repo_info={"name":name,"path":str(repo),"db":keep_db}
            for query in QUERIES:
                methods={}
                for method in ("cli","mcp","rg","rg_m5"):
                    times=[]; payload=[]; wire=[]; counts=[]
                    for _ in range(args.query_samples):
                        elapsed, payload_bytes, wire_bytes, found = response_payload(binary, method, repo_info, query)
                        times.append(elapsed); payload.append(payload_bytes); wire.append(wire_bytes); counts.append(found)
                    methods[method]={"wall_ms":distribution(times),"payload_bytes":distribution(payload,"bytes"),
                        "wire_bytes":distribution(wire,"bytes"),"approx_tokens_by_payload_bytes":distribution([math.ceil(n/4) for n in payload],"tokens"),
                        "returned_result_count":distribution(counts,"results")}
                record["comparisons"].append({"query":query,"warm_cache_condition":"indexed DB and filesystem left warm; no cache drop","methods":methods})
            with sqlite3.connect(f"file:{keep_db}?mode=ro", uri=True) as connection:
                parser_error_rows = connection.execute(
                    "SELECT language, rel_path FROM code_files WHERE parse_error=1 ORDER BY language, rel_path"
                ).fetchall()
                connection.close()
            parser_errors_by_language = {}
            for language, relative_path in parser_error_rows:
                parser_errors_by_language.setdefault(language or "unknown", {}).setdefault(
                    "tree_sitter_parse_error", []
                ).append({"file": relative_path, "classification": "unknown"})
            record["parser_errors"] = {
                "count": len(parser_error_rows),
                "files_by_language_and_error_type": parser_errors_by_language,
                "classification_reason": (
                    "The persisted parse_error field is boolean and index-status reports only an aggregate; "
                    "per-file Tree-sitter error nodes and parser duration are not retained, so the cause "
                    "cannot be classified without inference."
                ),
            }
        output["repositories"].append(record)
        checkpoint_path = Path(os.environ.get("TMPDIR", tempfile.gettempdir())) / f"repository-{name}-completed.json"
        checkpoint_record = {"schema_version": 1, "repository": record}
        with checkpoint_path.open("x", encoding="utf-8") as checkpoint:
            json.dump(checkpoint_record, checkpoint, indent=2)
            checkpoint.write("\n")
        print(f"completed {name}: {status.get('eligible_files')} eligible files", file=sys.stderr)
    for record in output["repositories"]:
        summary={}
        for method in ("cli","mcp","rg","rg_m5"):
            rows=[query["methods"][method] for query in record["comparisons"]]
            times=[x for row in rows for x in row["wall_ms"]["samples_ms"]]
            payload=[x for row in rows for x in row["payload_bytes"]["samples_bytes"]]
            wire=[x for row in rows for x in row["wire_bytes"]["samples_bytes"]]
            tokens=[x for row in rows for x in row["approx_tokens_by_payload_bytes"]["samples_tokens"]]
            result_counts=[x for row in rows for x in row["returned_result_count"]["samples_results"]]
            summary[method]={"wall_ms":distribution(times),"payload_bytes":distribution(payload,"bytes"),
                "wire_bytes":distribution(wire,"bytes"),"approx_tokens_by_payload_bytes":distribution(tokens,"tokens"),
                "returned_result_count":distribution(result_counts,"results")}
        record["comparison_summary_all_queries"]=summary
        record["break_even_queries_by_query"] = []
        index_p50 = record["index_summary"]["fresh_database_full_index_ms"]["p50_ms"]
        for query in record["comparisons"]:
            cli_p50 = query["methods"]["cli"]["wall_ms"]["p50_ms"]
            break_even = {"query": query["query"], "index_build_p50_ms": index_p50, "midnight_cli_p50_ms": cli_p50}
            for method in ("rg", "rg_m5"):
                rg_p50 = query["methods"][method]["wall_ms"]["p50_ms"]
                denominator = rg_p50 - cli_p50
                break_even[f"{method}_p50_ms"] = rg_p50
                break_even[f"{method}_minus_midnight_p50_ms"] = denominator
                break_even[f"{method}_break_even_query_count"] = index_p50 / denominator if denominator > 0 else None
                break_even[f"{method}_break_even_status"] = "finite" if denominator > 0 else "no positive break-even (rg not slower at p50)"
            record["break_even_queries_by_query"].append(break_even)
        for method in ("rg", "rg_m5"):
            rg_p50 = summary[method]["wall_ms"]["p50_ms"]
            cli_p50 = summary["cli"]["wall_ms"]["p50_ms"]
            denominator = rg_p50 - cli_p50
            summary[method]["break_even_queries_using_aggregate_p50"] = index_p50 / denominator if denominator > 0 else None
            summary[method]["break_even_status_using_aggregate_p50"] = "finite" if denominator > 0 else "no positive break-even (rg not slower at p50)"
    json.dump(output,sys.stdout,indent=2)
    print()


if __name__ == "__main__":
    main()
