#!/usr/bin/env python3
"""Run a bounded, immutable local AETHER Midnight performance campaign."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MEASURER = ROOT / "scripts" / "measure_process.py"
MANIFEST = ROOT / "benchmark" / "manifest.json"
MODES = {"smoke": 1, "standard": 5, "stress": 30}
MIB = 1024 * 1024


def command_output(args: list[str], timeout: float = 5.0) -> str | None:
    try:
        completed = subprocess.run(
            args, capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return completed.stdout.strip() if completed.returncode == 0 else None


def sysctl_value(name: str) -> str | None:
    return command_output(["/usr/sbin/sysctl", "-n", name])


def linux_memory_bytes() -> int | None:
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError):
        pass
    return None


def machine_snapshot() -> dict[str, object]:
    system = platform.system()
    cpu_model = None
    physical_cores = None
    logical_cores = os.cpu_count()
    memory_bytes = None
    cpu_features: dict[str, str | None] = {}
    filesystem: dict[str, object] = {}
    pressure: dict[str, object] = {}

    if system == "Darwin":
        cpu_model = sysctl_value("machdep.cpu.brand_string")
        physical_cores = int(sysctl_value("hw.physicalcpu") or 0) or None
        logical_cores = int(sysctl_value("hw.logicalcpu") or logical_cores or 0) or None
        memory_bytes = int(sysctl_value("hw.memsize") or 0) or None
        cpu_features = {
            "machdep.cpu.features": sysctl_value("machdep.cpu.features"),
            "machdep.cpu.leaf7_features": sysctl_value("machdep.cpu.leaf7_features"),
        }
        disk_info = command_output(["/usr/sbin/diskutil", "info", "-plist", "/"])
        # diskutil -plist is binary or XML; use its byte output below instead.
        try:
            raw = subprocess.run(
                ["/usr/sbin/diskutil", "info", "-plist", "/"],
                capture_output=True,
                timeout=5,
                check=True,
            ).stdout
            info = plistlib.loads(raw)
            filesystem = {
                key: info.get(key)
                for key in (
                    "FilesystemType",
                    "VolumeName",
                    "DeviceLocation",
                    "SolidState",
                    "BusProtocol",
                    "MediaName",
                )
                if info.get(key) is not None
            }
        except (OSError, subprocess.SubprocessError, ValueError):
            if disk_info:
                filesystem["diskutil_info"] = disk_info
        pressure = {
            "vm_stat": command_output(["/usr/bin/vm_stat"]),
            "swap_usage": sysctl_value("vm.swapusage"),
            "memory_pressure": command_output(["/usr/bin/memory_pressure", "-Q"]),
            "power_state": command_output(["/usr/bin/pmset", "-g", "batt"]),
            "thermal_state": command_output(["/usr/bin/pmset", "-g", "therm"]),
        }
    elif system == "Linux":
        try:
            cpuinfo = Path("/proc/cpuinfo").read_text()
            for line in cpuinfo.splitlines():
                if line.startswith("model name"):
                    cpu_model = line.partition(":")[2].strip()
                    break
        except OSError:
            pass
        logical_cores = logical_cores or 1
        memory_bytes = linux_memory_bytes()
        filesystem["mount"] = command_output(
            ["findmnt", "-n", "-o", "SOURCE,FSTYPE,TARGET", "--target", str(ROOT)]
        )
        filesystem["devices"] = command_output(
            ["lsblk", "-dn", "-o", "NAME,ROTA,TYPE,TRAN,MODEL"]
        )
        try:
            cpuinfo = Path("/proc/cpuinfo").read_text()
            first = cpuinfo.split("\n\n", 1)[0]
            features = next(
                (line.partition(":")[2].strip() for line in first.splitlines() if line.startswith("flags")),
                None,
            )
            cpu_features["flags"] = features
        except OSError:
            pass

    return {
        "os": system,
        "os_release": platform.release(),
        "os_version": platform.platform(),
        "architecture": platform.machine(),
        "cpu_model": cpu_model,
        "physical_cores": physical_cores,
        "logical_cores": logical_cores,
        "memory_bytes": memory_bytes,
        "cpu_features": cpu_features,
        "filesystem_and_storage": filesystem,
        "system_state_before_campaign": pressure,
        "storage_cache_note": (
            "Filesystem cache is not evicted; initial index uses a new SQLite database, not a cold OS cache."
        ),
    }


def get_git(root: Path, *args: str) -> str | None:
    return command_output(["git", "-C", str(root), *args])


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def distribution(values: list[float | int], unit: str) -> dict[str, object]:
    ordered = sorted(values)

    def percentile(p: float):
        if not ordered:
            return None
        return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)]

    return {
        "n": len(ordered),
        f"p50_{unit}": percentile(0.50),
        f"p95_{unit}": percentile(0.95),
        f"p99_{unit}": percentile(0.99),
        f"samples_{unit}": ordered,
    }


def database_bytes(path: Path) -> int:
    return sum(
        candidate.stat().st_size
        for candidate in (path, Path(f"{path}-wal"), Path(f"{path}-shm"))
        if candidate.exists()
    )


def measured(
    binary: Path,
    args: list[str],
    env: dict[str, str],
    stdin_text: str | None,
    interval_ms: int,
    watch_paths: list[Path] | None = None,
    stdin_hold_ms: int = 0,
) -> dict[str, object]:
    command = [str(binary), *args]
    wrapped = [
        sys.executable,
        str(MEASURER),
        "--interval-ms",
        str(interval_ms),
    ]
    if stdin_text is not None:
        wrapped.extend(["--stdin-text", stdin_text])
    if stdin_hold_ms > 0:
        wrapped.extend(["--stdin-hold-ms", str(stdin_hold_ms)])
    for path in watch_paths or []:
        wrapped.extend(["--watch-path", str(path)])
    wrapped.extend(["--", *command])
    completed = subprocess.run(
        wrapped,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(
            f"measurement wrapper exited {completed.returncode}: {completed.stderr[-2000:]}"
        )
    result = json.loads(completed.stdout)
    if result.get("measurement_error"):
        raise RuntimeError(result["measurement_error"])
    result["command"] = command
    return result


def save_json(path: Path, value: object) -> None:
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def run_cli(binary: Path, db: Path, *args: object, repo: Path | None = None) -> list[str]:
    command = [str(binary), "--db", str(db), *map(str, args)]
    if repo is not None:
        command.extend(["--repo", str(repo)])
    return command


def run_watched_process(
    command: list[str], db: Path, env: dict[str, str], interval_ms: int
) -> dict[str, object]:
    start = time.perf_counter()
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    watched = [db, Path(f"{db}-wal"), Path(f"{db}-shm")]
    peaks = {str(path): 0 for path in watched}
    while process.poll() is None:
        for path in watched:
            try:
                peaks[str(path)] = max(peaks[str(path)], path.stat().st_size)
            except OSError:
                pass
        time.sleep(max(interval_ms, 1) / 1000)
    stdout_bytes, stderr_bytes = process.communicate()
    for path in watched:
        try:
            peaks[str(path)] = max(peaks[str(path)], path.stat().st_size)
        except OSError:
            pass
    return {
        "returncode": process.returncode,
        "elapsed_ms": (time.perf_counter() - start) * 1000,
        "stdout": stdout_bytes.decode("utf-8", errors="replace"),
        "stderr_tail": stderr_bytes.decode("utf-8", errors="replace")[-4000:],
        "watched_file_peak_sizes_bytes": peaks,
    }


def parse_json_stdout(measurement: dict[str, object]) -> dict[str, object]:
    raw = str(measurement.get("stdout", ""))
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"measured command did not emit JSON: {raw[:500]!r}") from error


def expected_query_quality(binary: Path, db: Path, repo: Path, env: dict[str, str], repeats: int) -> dict[str, object]:
    suite = json.loads(MANIFEST.read_text(encoding="utf-8"))["queries"]
    details = []
    recalls = {1: [], 5: [], 10: []}
    reciprocal_ranks = []
    search_samples = []
    for query in suite:
        times = []
        observed: list[str] = []
        for _ in range(repeats):
            result = measured(
                binary,
                ["--db", str(db), "search", "--repo", str(repo), "--query", query["query"], "--limit", "10"],
                env,
                None,
                50,
            )
            if result["returncode"] != 0:
                raise RuntimeError(f"search failed for {query['id']}: {result['stderr_tail']}")
            response = parse_json_stdout(result)
            paths = [row.get("path") for row in response.get("results", [])]
            observed = paths
            times.append(float(result["elapsed_ms"]))
        relevant = set(query["relevant_paths"])
        ranks = [rank for rank, path in enumerate(observed, 1) if path in relevant]
        for cutoff in recalls:
            recalls[cutoff].append(len([rank for rank in ranks if rank <= cutoff]) / max(len(relevant), 1))
        reciprocal_ranks.append(1.0 / min(ranks) if ranks else 0.0)
        search_samples.extend(times)
        details.append(
            {
                "id": query["id"],
                "query": query["query"],
                "expected_paths": sorted(relevant),
                "top10_paths": observed,
                "found_expected": bool(ranks),
                "reciprocal_rank": 1.0 / min(ranks) if ranks else 0.0,
                "elapsed_ms": distribution(times, "ms"),
            }
        )
    return {
        "query_count": len(suite),
        "recall_at_1": sum(recalls[1]) / len(recalls[1]),
        "recall_at_5": sum(recalls[5]) / len(recalls[5]),
        "recall_at_10": sum(recalls[10]) / len(recalls[10]),
        "mrr": sum(reciprocal_ranks) / len(reciprocal_ranks),
        "warm_search_latency": distribution(search_samples, "ms"),
        "queries": details,
    }


def summary_for_index(samples: list[dict[str, object]], source_bytes: int) -> dict[str, object]:
    elapsed = [float(sample["elapsed_ms"]) for sample in samples if sample.get("returncode") == 0]
    rss = [int(sample["resident_highwater_bytes"]) for sample in samples if sample.get("resident_highwater_bytes")]
    footprint = [int(sample["phys_footprint_highwater_bytes"]) for sample in samples if sample.get("phys_footprint_highwater_bytes")]
    cpu = [float(sample["cpu_seconds"]) for sample in samples if sample.get("returncode") == 0]
    db = [int(sample["database_bytes"]) for sample in samples if sample.get("database_bytes") is not None]
    files = [int(sample["eligible_files"]) for sample in samples if sample.get("eligible_files") is not None]
    source = [int(sample["eligible_bytes"]) for sample in samples if sample.get("eligible_bytes") is not None]
    result: dict[str, object] = {
        "full_index_wall": distribution(elapsed, "ms"),
        "resident_highwater": distribution(rss, "bytes"),
        "mac_phys_footprint_highwater": distribution(footprint, "bytes"),
        "cpu_seconds": distribution(cpu, "seconds"),
        "database_bytes_main_wal_shm": distribution(db, "bytes"),
        "eligible_files": distribution(files, "files"),
        "eligible_source_bytes": distribution(source, "bytes"),
        "cpu_seconds_per_gib_source": distribution(
            [value / max(source_bytes / (1024**3), 1e-9) for value in cpu], "cpu_seconds_per_gib"
        ),
        "files_per_second": distribution(
            [file_count / max(duration / 1000, 1e-9) for file_count, duration in zip(files, elapsed)],
            "files_per_second",
        ),
        "mib_per_second": distribution(
            [byte_count / MIB / max(duration / 1000, 1e-9) for byte_count, duration in zip(source, elapsed)],
            "mib_per_second",
        ),
        "memory_per_indexed_file_bytes": distribution(
            [value / max(file_count, 1) for value, file_count in zip(rss, files)], "bytes_per_file"
        ),
        "database_bytes_per_gib_source": distribution(
            [size / max(byte_count / (1024**3), 1e-9) for size, byte_count in zip(db, source)],
            "database_bytes_per_gib_source",
        ),
        "cpu_cores": distribution(
            [
                float(sample["cpu_seconds"]) / max(float(sample["elapsed_ms"]) / 1000, 1e-9)
                for sample in samples
                if sample.get("returncode") == 0
            ],
            "cores",
        ),
        "catalog_complete_all_samples": bool(samples) and all(sample.get("catalog_complete") is True for sample in samples),
        "structural_complete_all_samples": bool(samples) and all(sample.get("structural_complete") is True for sample in samples),
        "successful_samples": sum(1 for sample in samples if sample.get("returncode") == 0),
        "failed_samples": sum(1 for sample in samples if sample.get("returncode") != 0),
    }
    return result


def worker_list(value: str) -> list[int]:
    workers = []
    for part in value.split(","):
        count = int(part.strip())
        if not 1 <= count <= 12:
            raise argparse.ArgumentTypeError("workers must be between 1 and 12")
        if count not in workers:
            workers.append(count)
    if not workers:
        raise argparse.ArgumentTypeError("at least one worker count is required")
    return workers


def run_campaign(args: argparse.Namespace) -> Path:
    root = args.repo.resolve() if args.repo else None
    if root is not None and (not root.is_dir() or not root.is_absolute()):
        raise RuntimeError(f"repository path is not a local directory: {root}")
    binary = args.binary.resolve()
    if not binary.is_file():
        raise RuntimeError(f"release binary not found; build it first: {binary}")
    samples_per_config = args.samples or MODES[args.mode]
    if not 1 <= samples_per_config <= 100:
        raise RuntimeError("sample count must be between 1 and 100")
    if args.interval_ms <= 0 or args.max_duration_s <= 0:
        raise RuntimeError("sampling interval and maximum duration must be positive")

    commit = get_git(ROOT, "rev-parse", "HEAD") or "unknown"
    branch = get_git(ROOT, "branch", "--show-current") or "unknown"
    started = dt.datetime.now(dt.timezone.utc)
    run_id = f"{started.strftime('%Y%m%dT%H%M%SZ')}_{commit[:12]}_{args.mode}_{uuid.uuid4().hex[:8]}"
    args.output_root.mkdir(parents=True, exist_ok=True)
    run_dir = args.output_root / run_id
    run_dir.mkdir()
    result: dict[str, object] = {}

    try:
        version = command_output([str(binary), "--version"])
        diagnose_db = run_dir / "diagnose.sqlite3"
        diagnose = subprocess.run(
            [str(binary), "--db", str(diagnose_db), "diagnose"],
            capture_output=True,
            text=True,
            check=True,
        )
        sqlite_version = json.loads(diagnose.stdout).get("sqlite_version")
        for suffix in ("", "-wal", "-shm"):
            Path(f"{diagnose_db}{suffix}").unlink(missing_ok=True)

        environment = os.environ.copy()
        environment["MIDNIGHT_INDEX_WORKERS"] = str(args.update_workers)
        environment.pop("MIDNIGHT_BENCH_BATCH_FILES", None)
        environment.pop("MIDNIGHT_BENCH_BATCH_BYTES", None)
        deadline = time.monotonic() + args.max_duration_s
        result: dict[str, object] = {
            "schema_version": 1,
            "run_id": run_id,
            "status": "running",
            "mode": args.mode,
            "sample_count_per_measurement": samples_per_config,
            "measurement": {
                "timer": "Python monotonic wall clock around one direct aether-midnight child",
                "sampling_interval_ms": args.interval_ms,
                "process_scope": "aether-midnight direct child; Python wrapper excluded; no descendants",
                "mac_memory_metric": "proc_pid_rusage(RUSAGE_INFO_V4) phys_footprint and lifetime_max_phys_footprint; ru_maxrss RSS separately",
                "linux_memory_metric": "/proc VmRSS/VmHWM plus periodic smaps_rollup PSS/private/anonymous and cgroup counters when readable",
                "mac_file_cache_and_pressure": "vm_stat, vm.swapusage, memory_pressure, power and thermal snapshots before/after; host-wide, not process-attributed",
                "cache_condition": "fresh SQLite database per full-index sample; OS file cache is left intact; no cold-OS-cache claim",
                "percentiles": "sorted nearest-index ceil((n-1)*p), matching committed benchmark conventions",
            },
            "environment": {
                "commit": commit,
                "branch": branch,
                "rustc": command_output(["rustc", "--version"]),
                "cargo": command_output(["cargo", "--version"]),
                "python": sys.version,
                "binary_version": version,
                "binary_sha256": sha256_file(binary),
                "cargo_lock_sha256": sha256_file(ROOT / "Cargo.lock"),
                "sqlite_version": sqlite_version,
                "hardware": machine_snapshot(),
            },
            "configuration": {
                "binary": str(binary),
                "repository": str(root) if root else "generated deterministic fixture",
                "worker_counts": args.workers,
                "update_workers": args.update_workers,
                "requested_samples": samples_per_config,
                "max_duration_s": args.max_duration_s,
                "index_environment": {
                    key: environment.get(key)
                    for key in ("MIDNIGHT_INDEX_WORKERS", "MIDNIGHT_IDENTIFIER_DECOMPOSITION")
                },
                "commands": [],
            },
            "fixture": None,
            "measurements": {},
            "failures": [],
            "limitations": [
                "Native macOS private footprint is not interchangeable with Linux RSS or cgroup memory.",
                "Process RSS high-water and physical-footprint high-water are reported as separate fields.",
                "No allocator-live-heap metric is available in this build.",
                "Per-phase timings come from IndexReport; OS memory samples are process-wide, not phase-attributed.",
            ],
        }
        save_json(run_dir / "manifest.json", result)

        with tempfile.TemporaryDirectory(prefix="midnight-performance-") as temporary:
            temp_root = Path(temporary)
            if root is None:
                corpus = temp_root / "corpus"
                generated = subprocess.run(
                    [sys.executable, str(ROOT / "scripts" / "generate_fixture.py"), "--output", str(corpus)],
                    capture_output=True,
                    text=True,
                    check=True,
                )
                result["fixture"] = json.loads(generated.stdout)
                fixture_manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
                queries = fixture_manifest["queries"]
                source_bytes = fixture_manifest["fixture"]["eligible_bytes"]
            else:
                corpus = root
                source_bytes = sum(path.stat().st_size for path in root.rglob("*") if path.is_file())
                queries = [{"id": "smoke", "query": "TODO", "relevant_paths": []}]
                result["fixture"] = {
                    "root": str(root),
                    "tracked_commit": get_git(root, "rev-parse", "HEAD"),
                    "eligible_source_bytes_on_disk": source_bytes,
                    "mode": "read-only repository measurement; source is not modified",
                }

            help_samples = []
            for _ in range(samples_per_config):
                sample = measured(binary, ["--version"], environment, None, args.interval_ms)
                help_samples.append(sample)
            result["measurements"]["startup"] = {
                "definition": "fresh process launch through --version output",
                "wall": distribution([float(sample["elapsed_ms"]) for sample in help_samples], "ms"),
                "raw_samples": help_samples,
            }

            first_context_samples = []
            for index in range(samples_per_config):
                db = temp_root / f"first-context-{index}.sqlite3"
                sample = measured(
                    binary,
                    ["--db", str(db), "search", "--repo", str(corpus), "--query", queries[0]["query"], "--limit", "5"],
                    environment,
                    None,
                    args.interval_ms,
                )
                response = parse_json_stdout(sample) if sample["returncode"] == 0 else {}
                rows = response.get("results", [])
                sample["first_result_count"] = len(rows)
                sample["grounded_result"] = bool(rows) and all(row.get("path") for row in rows)
                sample["fresh_unindexed"] = any(row.get("freshness") == "fresh_unindexed" for row in rows)
                sample["index_complete"] = all(row.get("index_complete") is False for row in rows)
                first_context_samples.append(sample)
                for suffix in ("", "-wal", "-shm"):
                    Path(f"{db}{suffix}").unlink(missing_ok=True)
            result["measurements"]["first_useful_context"] = {
                "definition": "fresh process search against a new empty SQLite database; OS cache is not cleared",
                "wall": distribution([float(sample["elapsed_ms"]) for sample in first_context_samples], "ms"),
                "all_samples_grounded": all(sample.get("grounded_result") for sample in first_context_samples),
                "all_samples_explicitly_unindexed": all(sample.get("fresh_unindexed") for sample in first_context_samples),
                "raw_samples": first_context_samples,
            }

            index_groups = []
            selected_db = None
            for worker_count in args.workers:
                group_samples = []
                for sample_index in range(samples_per_config):
                    if time.monotonic() >= deadline:
                        result["failures"].append(
                            {"kind": "duration_limit", "worker_count": worker_count, "completed_samples": sample_index}
                        )
                        break
                    db = temp_root / f"index-w{worker_count}-s{sample_index}.sqlite3"
                    sample_env = environment.copy()
                    sample_env["MIDNIGHT_INDEX_WORKERS"] = str(worker_count)
                    result["configuration"]["commands"].append(
                        {
                            "command": [str(binary), "--db", str(db), "index", "--repo", str(corpus)],
                            "env": {"MIDNIGHT_INDEX_WORKERS": str(worker_count)},
                            "sample": sample_index,
                        }
                    )
                    sample = measured(
                        binary,
                        ["--db", str(db), "index", "--repo", str(corpus)],
                        sample_env,
                        None,
                        args.interval_ms,
                        [Path(f"{db}{suffix}") for suffix in ("", "-wal", "-shm")],
                    )
                    report = {}
                    if sample["returncode"] == 0:
                        try:
                            report = parse_json_stdout(sample)
                        except RuntimeError as error:
                            sample["parse_error"] = str(error)
                    sample["worker_count"] = worker_count
                    sample["eligible_files"] = report.get("eligible_files")
                    sample["eligible_bytes"] = report.get("eligible_bytes")
                    sample["complete"] = report.get("complete")
                    sample["catalog_complete"] = report.get("catalog_complete")
                    sample["structural_complete"] = report.get("structural_complete")
                    sample["phase_timings_ms"] = report.get("timings_ms")
                    sample["pipeline_counters"] = {
                        key: report.get(key)
                        for key in (
                            "max_in_flight_bytes",
                            "max_pipeline_buffered_bytes",
                            "max_queued_artifacts",
                            "max_pipeline_artifacts",
                            "worker_count",
                            "batch_file_limit",
                            "batch_byte_limit",
                        )
                    }
                    sample["database_bytes"] = database_bytes(db)
                    sample["database_peak_components_bytes"] = sample.get("watched_file_peak_sizes_bytes", {})
                    sample["_database_path"] = str(db)
                    sample["resident_highwater_bytes"] = (
                        sample.get("sampled_peaks", {}).get("vmhwm_bytes")
                        or sample.get("ru_maxrss_bytes")
                    )
                    sample["phys_footprint_highwater_bytes"] = sample.get("sampled_peaks", {}).get(
                        "lifetime_max_phys_footprint_bytes"
                    )
                    sample["index_pass"] = bool(
                        sample["returncode"] == 0
                        and sample.get("catalog_complete") is True
                        and sample.get("structural_complete") is True
                    )
                    if sample["returncode"] != 0 or not sample.get("catalog_complete"):
                        result["failures"].append(
                            {
                                "kind": "index_failure_or_incomplete_catalog",
                                "worker_count": worker_count,
                                "sample": sample_index,
                                "returncode": sample["returncode"],
                                "stderr_tail": sample.get("stderr_tail"),
                                "catalog_complete": sample.get("catalog_complete"),
                            }
                        )
                    elif sample.get("structural_complete") is not True:
                        result["failures"].append(
                            {
                                "kind": "structural_index_incomplete",
                                "worker_count": worker_count,
                                "sample": sample_index,
                                "eligible_files": sample.get("eligible_files"),
                                "phase_timings_ms": sample.get("phase_timings_ms"),
                            }
                        )
                    group_samples.append(sample)
                    if worker_count == args.update_workers and sample_index == samples_per_config - 1 and sample["returncode"] == 0:
                        selected_db = db
                group = {
                    "worker_count": worker_count,
                    "raw_samples": group_samples,
                    "summary": summary_for_index(group_samples, source_bytes),
                }
                index_groups.append(group)
                result["measurements"]["full_index_worker_screen"] = index_groups
                save_json(run_dir / "manifest.json", result)

            if selected_db is None:
                successful = [
                    sample
                    for group in index_groups
                    for sample in group["raw_samples"]
                    if sample.get("returncode") == 0 and sample.get("catalog_complete") is True
                ]
                if successful:
                    selected = min(successful, key=lambda sample: float(sample["elapsed_ms"]))
                    selected_db = Path(selected["_database_path"])
            for group in index_groups:
                for sample in group["raw_samples"]:
                    sample.pop("_database_path", None)

            retrieval = None
            if selected_db is not None and root is None:
                retrieval = expected_query_quality(
                    binary, selected_db, corpus, environment, samples_per_config
                )
            result["measurements"]["retrieval_quality"] = retrieval or {
                "status": "not run: retrieval ground truth is defined only for the generated fixed fixture"
            }

            warm_samples = []
            if selected_db is not None:
                warm_query = queries[0]["query"]
                for _ in range(samples_per_config):
                    sample = measured(
                        binary,
                        ["--db", str(selected_db), "search", "--repo", str(corpus), "--query", warm_query, "--limit", "5"],
                        environment,
                        None,
                        args.interval_ms,
                    )
                    if sample["returncode"] == 0:
                        response = parse_json_stdout(sample)
                        sample["result_count"] = len(response.get("results", []))
                        sample["grounded"] = all(row.get("path") for row in response.get("results", []))
                    warm_samples.append(sample)
            result["measurements"]["warm_retrieval"] = {
                "definition": "fresh CLI process, completed index and warm SQLite/filesystem state",
                "wall": distribution([float(sample["elapsed_ms"]) for sample in warm_samples], "ms"),
                "raw_samples": warm_samples,
            }

            mcp_samples = []
            if selected_db is not None:
                for index in range(samples_per_config):
                    request = "\n".join(
                        [
                            json.dumps(
                                {
                                    "jsonrpc": "2.0",
                                    "id": 1,
                                    "method": "initialize",
                                    "params": {
                                        "protocolVersion": "2025-11-25",
                                        "capabilities": {},
                                        "clientInfo": {"name": "performance-campaign", "version": "1"},
                                    },
                                }
                            ),
                            json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                            json.dumps(
                                {
                                    "jsonrpc": "2.0",
                                    "id": 2,
                                    "method": "tools/call",
                                    "params": {
                                        "name": "search_code",
                                        "arguments": {
                                            "repo_path": str(corpus),
                                            "query": queries[0]["query"],
                                            "limit": 5,
                                        },
                                    },
                                }
                            ),
                            "",
                        ]
                    )
                    sample = measured(
                        binary,
                        ["--db", str(selected_db), "mcp"],
                        environment,
                        request,
                        args.interval_ms,
                    )
                    responses = []
                    if sample["returncode"] == 0:
                        try:
                            responses = [json.loads(line) for line in sample["stdout"].splitlines() if line.strip()]
                            sample["initialize_ok"] = any("result" in item and item.get("id") == 1 for item in responses)
                            sample["tools_call_ok"] = any("result" in item and item.get("id") == 2 for item in responses)
                            sample["response_count"] = len(responses)
                        except json.JSONDecodeError as error:
                            sample["protocol_parse_error"] = str(error)
                    mcp_samples.append(sample)
            result["measurements"]["mcp_round_trip"] = {
                "definition": "fresh MCP process, initialize and one tools/call search_code round trip",
                "wall": distribution([float(sample["elapsed_ms"]) for sample in mcp_samples], "ms"),
                "all_samples_protocol_success": bool(mcp_samples)
                and all(sample.get("initialize_ok") and sample.get("tools_call_ok") for sample in mcp_samples),
                "raw_samples": mcp_samples,
            }

            mcp_idle_samples = []
            if selected_db is not None:
                initialize_request = "\n".join(
                    [
                        json.dumps(
                            {
                                "jsonrpc": "2.0",
                                "id": 1,
                                "method": "initialize",
                                "params": {
                                    "protocolVersion": "2025-11-25",
                                    "capabilities": {},
                                    "clientInfo": {"name": "performance-idle", "version": "1"},
                                },
                            }
                        ),
                        json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                        "",
                    ]
                )
                for _ in range(samples_per_config):
                    sample = measured(
                        binary,
                        ["--db", str(selected_db), "mcp"],
                        environment,
                        initialize_request,
                        args.interval_ms,
                        stdin_hold_ms=2000,
                    )
                    responses = []
                    if sample["returncode"] == 0:
                        try:
                            responses = [json.loads(line) for line in sample["stdout"].splitlines() if line.strip()]
                            sample["initialized"] = any(
                                item.get("id") == 1 and "result" in item for item in responses
                            )
                        except json.JSONDecodeError as error:
                            sample["protocol_parse_error"] = str(error)
                    sample["idle_hold_ms"] = 2000
                    sample["idle_cpu_percent_one_core"] = (
                        100 * float(sample["cpu_seconds"]) / max(float(sample["elapsed_ms"]) / 1000, 1e-9)
                    )
                    sample["resident_highwater_bytes"] = (
                        sample.get("sampled_peaks", {}).get("vmhwm_bytes")
                        or sample.get("ru_maxrss_bytes")
                    )
                    sample["phys_footprint_highwater_bytes"] = sample.get("sampled_peaks", {}).get(
                        "lifetime_max_phys_footprint_bytes"
                    )
                    mcp_idle_samples.append(sample)
            result["measurements"]["mcp_idle_session"] = {
                "definition": "initialized MCP process kept alive with stdin open and no requests for 2 seconds",
                "idle_cpu_percent_one_core": distribution(
                    [float(sample["idle_cpu_percent_one_core"]) for sample in mcp_idle_samples], "percent"
                ),
                "resident_highwater_bytes": distribution(
                    [int(sample["resident_highwater_bytes"]) for sample in mcp_idle_samples if sample.get("resident_highwater_bytes")],
                    "bytes",
                ),
                "mac_phys_footprint_highwater_bytes": distribution(
                    [int(sample["phys_footprint_highwater_bytes"]) for sample in mcp_idle_samples if sample.get("phys_footprint_highwater_bytes")],
                    "bytes",
                ),
                "all_samples_initialized": bool(mcp_idle_samples)
                and all(sample.get("initialized") for sample in mcp_idle_samples),
                "raw_samples": mcp_idle_samples,
            }

            incremental = {"status": "skipped: benchmark does not modify a caller-provided repository"}
            if root is None and selected_db is not None:
                target = corpus / "src" / "00000.rs"
                original = target.read_bytes()
                original_stat = target.stat()
                incremental_samples = []
                try:
                    for index in range(samples_per_config):
                        marker = f"midnight_visibility_token_{index:05d}"
                        target.write_bytes(original + f"\n// {marker}\n".encode("ascii"))
                        started_update = time.perf_counter()
                        index_sample = run_watched_process(
                            [str(binary), "--db", str(selected_db), "index", "--repo", str(corpus)],
                            selected_db,
                            environment,
                            args.interval_ms,
                        )
                        index_report = (
                            json.loads(index_sample["stdout"])
                            if index_sample["returncode"] == 0
                            else {}
                        )
                        search_started = time.perf_counter()
                        query_process = subprocess.run(
                            [
                                str(binary),
                                "--db",
                                str(selected_db),
                                "search",
                                "--repo",
                                str(corpus),
                                "--query",
                                marker,
                                "--limit",
                                "5",
                            ],
                            capture_output=True,
                            text=True,
                            env=environment,
                            check=False,
                        )
                        query_sample = {
                            "returncode": query_process.returncode,
                            "elapsed_ms": (time.perf_counter() - search_started) * 1000,
                            "stdout": query_process.stdout,
                            "stderr_tail": query_process.stderr[-4000:],
                        }
                        query_response = json.loads(query_sample["stdout"]) if query_process.returncode == 0 else {}
                        visible = any(
                            row.get("path") == "src/00000.rs" and row.get("freshness") == "fresh"
                            for row in query_response.get("results", [])
                        )
                        incremental_samples.append(
                            {
                                "marker": marker,
                                "elapsed_ms": (time.perf_counter() - started_update) * 1000,
                                "index": index_sample,
                                "index_report": {
                                    key: index_report.get(key)
                                    for key in (
                                        "complete",
                                        "catalog_complete",
                                        "structural_complete",
                                        "eligible_files",
                                        "eligible_bytes",
                                        "timings_ms",
                                        "max_pipeline_buffered_bytes",
                                    )
                                },
                                "search": query_sample,
                                "visible_as_fresh": visible,
                                "database_bytes": database_bytes(selected_db),
                                "database_peak_components_bytes": index_sample.get(
                                    "watched_file_peak_sizes_bytes", {}
                                ),
                            }
                        )
                finally:
                    target.write_bytes(original)
                    os.utime(target, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
                    restored = measured(
                        binary,
                        ["--db", str(selected_db), "index", "--repo", str(corpus)],
                        environment,
                        None,
                        args.interval_ms,
                    )
                    result["measurements"]["incremental_update"] = {
                        "raw_samples": incremental_samples,
                        "wall": distribution([sample["elapsed_ms"] for sample in incremental_samples], "ms"),
                        "all_updates_visible_as_fresh": bool(incremental_samples)
                        and all(sample["visible_as_fresh"] for sample in incremental_samples),
                        "restoration_index_returncode": restored["returncode"],
                        "source_restored_byte_for_byte": target.read_bytes() == original,
                    }

        result["status"] = "completed_with_failures" if result["failures"] else "completed"
        result["finished_at_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
        result["elapsed_campaign_s"] = time.monotonic() - (deadline - args.max_duration_s)
        result["system_state_after_campaign"] = machine_snapshot().get("system_state_before_campaign")
        save_json(run_dir / "manifest.json", result)
        save_json(run_dir / "summary.json", {
            "run_id": run_id,
            "status": result["status"],
            "commit": commit,
            "branch": branch,
            "mode": args.mode,
            "worker_screen": [
                {
                    "worker_count": group["worker_count"],
                    **group["summary"],
                }
                for group in index_groups
            ],
            "startup": result["measurements"].get("startup", {}).get("wall"),
            "first_useful_context": result["measurements"].get("first_useful_context", {}).get("wall"),
            "retrieval_quality": result["measurements"].get("retrieval_quality"),
            "warm_retrieval": result["measurements"].get("warm_retrieval", {}).get("wall"),
            "incremental_update": result["measurements"].get("incremental_update"),
            "mcp_round_trip": result["measurements"].get("mcp_round_trip", {}).get("wall"),
            "failures": result["failures"],
        })
        return run_dir
    except BaseException as error:
        result = result or {}
        result["status"] = "failed"
        result["failure"] = {"type": type(error).__name__, "message": str(error)}
        result["finished_at_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
        save_json(run_dir / "manifest.json", result)
        raise


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=sorted(MODES), default="smoke")
    parser.add_argument("--samples", type=int, help="override mode sample count (1..100)")
    parser.add_argument("--workers", type=worker_list, default=worker_list("1,2,4,6,8,12"))
    parser.add_argument("--update-workers", type=int, default=min(4, os.cpu_count() or 1))
    parser.add_argument("--repo", type=Path, help="optional caller-owned local repository; it is never modified")
    parser.add_argument("--binary", type=Path, default=ROOT / "target" / "release" / "aether-midnight")
    parser.add_argument("--output-root", type=Path, default=ROOT / "benchmark" / "runs")
    parser.add_argument("--interval-ms", type=int, default=50)
    parser.add_argument("--max-duration-s", type=int, default=1800)
    args = parser.parse_args()
    if args.update_workers < 1 or args.update_workers > 12:
        parser.error("--update-workers must be between 1 and 12")
    return args


def main() -> int:
    args = parse_args()
    try:
        run_dir = run_campaign(args)
    except Exception as error:
        print(f"performance campaign failed: {error}", file=sys.stderr)
        return 1
    summary = json.loads((run_dir / "summary.json").read_text(encoding="utf-8"))
    print(json.dumps({"run_dir": str(run_dir), "status": summary["status"], "worker_screen": summary["worker_screen"]}, indent=2))
    return 0 if summary["status"] == "completed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
