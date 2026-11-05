#!/usr/bin/env python3
"""Run bounded CPU-only AETHER Midnight campaigns on Modal Sandboxes."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import subprocess
import sys
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TIERS = {
    "constrained": {"cpu_cores": 0.5, "memory_mib": 256, "workers": "1,2"},
    "minimal": {"cpu_cores": 1.0, "memory_mib": 512, "workers": "1,2"},
    "balanced": {"cpu_cores": 2.0, "memory_mib": 1024, "workers": "1,2,4"},
    "performance": {"cpu_cores": 4.0, "memory_mib": 2048, "workers": "2,4,6"},
    "high_parallelism": {"cpu_cores": 8.0, "memory_mib": 4096, "workers": "4,8"},
}
# Current Modal Sandbox non-preemptible rates, USD per physical core-second and GiB-second.
CPU_USD_PER_CORE_SECOND = 0.00003942
MEMORY_USD_PER_GIB_SECOND = 0.00000667


def source_fingerprint() -> dict[str, object]:
    paths = [ROOT / "src", ROOT / "scripts", ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "benchmark" / "manifest.json"]
    files = []
    for path in paths:
        if path.is_dir():
            files.extend(
                candidate
                for candidate in path.rglob("*")
                if candidate.is_file() and "__pycache__" not in candidate.parts and candidate.suffix != ".pyc"
            )
        elif path.is_file():
            files.append(path)
    digest = hashlib.sha256()
    for path in sorted(set(files)):
        relative = path.relative_to(ROOT).as_posix().encode()
        digest.update(len(relative).to_bytes(4, "big"))
        digest.update(relative)
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)

    def git(args: list[str]) -> bytes:
        try:
            return subprocess.run(
                ["git", "-C", str(ROOT), *args], capture_output=True, check=False
            ).stdout
        except OSError:
            return b""

    status = git(["status", "--porcelain", "-z", "--untracked-files=all"])
    diff = git(["diff", "HEAD", "--binary"])
    return {
        "commit": git(["rev-parse", "HEAD"]).decode().strip() or "unknown",
        "branch": git(["branch", "--show-current"]).decode().strip() or "unknown",
        "dirty": bool(status),
        "status_entries": [entry.decode("utf-8", errors="replace") for entry in status.split(b"\0") if entry],
        "diff_sha256": hashlib.sha256(diff).hexdigest(),
        "uploaded_source_tree_sha256": digest.hexdigest(),
        "uploaded_paths": [path.relative_to(ROOT).as_posix() for path in sorted(set(files))],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("smoke", "standard", "stress"), default="smoke")
    parser.add_argument("--tier", action="append", choices=sorted(TIERS), help="repeat to select tiers; defaults to all tiers")
    parser.add_argument("--workers", help="override the screening worker set for each selected tier")
    parser.add_argument("--samples", type=int, help="override mode samples per worker configuration")
    parser.add_argument("--budget-usd", type=float, default=10.0)
    parser.add_argument("--image-build-reserve-usd", type=float, default=5.0)
    parser.add_argument("--container-timeout-s", type=int, default=600)
    parser.add_argument("--max-duration-s", type=int, default=480)
    parser.add_argument("--output-root", type=Path, default=ROOT / "benchmark" / "runs" / "modal")
    parser.add_argument("--dry-run", action="store_true", help="print the exact resource/cost plan without using Modal")
    args = parser.parse_args()
    if args.samples is not None and not 1 <= args.samples <= 100:
        parser.error("--samples must be between 1 and 100")
    if args.container_timeout_s < 1 or args.max_duration_s < 1:
        parser.error("timeouts must be positive")
    if args.container_timeout_s < args.max_duration_s:
        parser.error("container timeout must be at least the benchmark max duration")
    if args.budget_usd <= 0 or args.image_build_reserve_usd < 0:
        parser.error("budget must be positive and image build reserve cannot be negative")
    args.tiers = args.tier or list(TIERS)
    if args.workers:
        try:
            worker_values = [int(value) for value in args.workers.split(",")]
        except ValueError:
            parser.error("--workers must be comma-separated integers")
        if not worker_values or any(value < 1 or value > 12 for value in worker_values):
            parser.error("--workers values must be between 1 and 12")
        args.worker_override = ",".join(str(value) for value in dict.fromkeys(worker_values))
    else:
        args.worker_override = None
    return args


def sample_count(args: argparse.Namespace) -> int:
    if args.samples is not None:
        return args.samples
    return {"smoke": 1, "standard": 5, "stress": 30}[args.mode]


def prior_run_reservations(output_root: Path) -> tuple[float, list[str]]:
    reserved = 0.0
    artifacts = []
    if not output_root.exists():
        return reserved, artifacts
    for path in sorted(output_root.glob("*/result.json")):
        try:
            result = json.loads(path.read_text())
            plan = result.get("plan", {})
            amount = plan.get("current_run_reservation_usd", plan.get("max_estimated_total_usd"))
            if isinstance(amount, (int, float)) and amount > 0:
                reserved += float(amount)
                artifacts.append(str(path))
        except (OSError, json.JSONDecodeError):
            continue
    return reserved, artifacts


def campaign_plan(args: argparse.Namespace) -> dict[str, object]:
    seconds = args.container_timeout_s
    tiers = []
    compute_estimate = 0.0
    for name in args.tiers:
        spec = TIERS[name]
        memory_gib = spec["memory_mib"] / 1024
        cpu_cost = spec["cpu_cores"] * seconds * CPU_USD_PER_CORE_SECOND
        memory_cost = memory_gib * seconds * MEMORY_USD_PER_GIB_SECOND
        worker_values = args.worker_override or spec["workers"]
        tiers.append(
            {
                "name": name,
                **spec,
                "worker_counts": [int(value) for value in worker_values.split(",")],
                "cpu_limit_cores": spec["cpu_cores"],
                "memory_limit_mib": spec["memory_mib"],
                "max_sandbox_compute_cost_usd": round(cpu_cost + memory_cost, 6),
            }
        )
        compute_estimate += cpu_cost + memory_cost
    current_reservation = compute_estimate + args.image_build_reserve_usd
    prior_reservations, prior_artifacts = prior_run_reservations(args.output_root)
    worst_case = prior_reservations + current_reservation
    return {
        "modal_sandbox_rates_usd": {
            "cpu_per_physical_core_second": CPU_USD_PER_CORE_SECOND,
            "memory_per_gib_second": MEMORY_USD_PER_GIB_SECOND,
            "basis": "Modal Sandbox non-preemptible list price; conservative reservation x full sandbox timeout",
        },
        "sample_count_per_worker_configuration": sample_count(args),
        "mode": args.mode,
        "sandbox_timeout_s": args.container_timeout_s,
        "benchmark_max_duration_s_per_sandbox": args.max_duration_s,
        "tiers": tiers,
        "max_estimated_sandbox_compute_usd": round(compute_estimate, 6),
        "image_build_and_control_plane_reserve_usd": args.image_build_reserve_usd,
        "current_run_reservation_usd": round(current_reservation, 6),
        "prior_campaign_reservations_usd": round(prior_reservations, 6),
        "prior_run_artifacts_counted": prior_artifacts,
        "max_estimated_total_usd": round(worst_case, 6),
        "configured_budget_usd": args.budget_usd,
        "within_budget": worst_case <= args.budget_usd,
        "gpu": None,
        "scratch_storage": "/tmp and image-local container SSD; no Modal Volume on the measured SQLite path",
    }


def make_image(modal):
    image = modal.Image.debian_slim(python_version="3.11").apt_install(
        "build-essential", "curl", "pkg-config"
    )
    image = image.add_local_dir(ROOT / "src", "/opt/midnight/src", copy=True)
    image = image.add_local_dir(
        ROOT / "scripts",
        "/opt/midnight/scripts",
        copy=True,
        ignore=["__pycache__", "*.pyc"],
    )
    image = image.add_local_file(ROOT / "Cargo.toml", "/opt/midnight/Cargo.toml", copy=True)
    image = image.add_local_file(ROOT / "Cargo.lock", "/opt/midnight/Cargo.lock", copy=True)
    image = image.add_local_file(
        ROOT / "benchmark" / "manifest.json", "/opt/midnight/benchmark/manifest.json", copy=True
    )
    image = image.run_commands(
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal",
        "bash -lc 'cd /opt/midnight && source /root/.cargo/env && rustc --version && cargo build --release --locked'",
    )
    return image


def read_stream(stream) -> str:
    value = stream.read()
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)


def run_modal(args: argparse.Namespace, plan: dict[str, object], run_dir: Path) -> dict[str, object]:
    try:
        import modal
    except ImportError as error:
        raise RuntimeError("Modal Python SDK is not installed; install the current SDK first") from error

    app_name = "aether-midnight-cpu-bench"
    try:
        app = modal.App.lookup(app_name, create_if_missing=True)
    except TypeError:
        # Compatibility with SDK releases where app lookup takes a positional flag.
        app = modal.App.lookup(app_name, True)
    image = make_image(modal)
    results = []
    for tier in plan["tiers"]:
        sandbox = None
        started = dt.datetime.now(dt.timezone.utc)
        record = {
            "tier": tier,
            "started_at_utc": started.isoformat(),
            "sdk_version": getattr(modal, "__version__", "unknown"),
        }
        command = [
            "python3",
            "/opt/midnight/scripts/modal_bench_worker.py",
            "--mode",
            args.mode,
            "--samples",
            str(sample_count(args)),
            "--workers",
            ",".join(map(str, tier["worker_counts"])),
            "--update-workers",
            str(max(1, min(4, int(tier["cpu_cores"])))),
            "--output-root",
            "/tmp/midnight-modal-runs",
            "--binary",
            "/opt/midnight/target/release/aether-midnight",
            "--max-duration-s",
            str(args.max_duration_s),
        ]
        record["command"] = command
        try:
            sandbox = modal.Sandbox.create(
                *command,
                app=app,
                image=image,
                cpu=(tier["cpu_cores"], tier["cpu_limit_cores"]),
                memory=(tier["memory_mib"], tier["memory_limit_mib"]),
                timeout=args.container_timeout_s,
                idle_timeout=args.container_timeout_s,
                workdir="/opt/midnight",
                block_network=True,
                env={
                    "MIDNIGHT_MODAL_TIER": tier["name"],
                    "MIDNIGHT_SOURCE_COMMIT": str(plan["source"]["commit"]),
                    "MIDNIGHT_SOURCE_BRANCH": str(plan["source"]["branch"]),
                    "MIDNIGHT_SOURCE_DIRTY": str(plan["source"]["dirty"]).lower(),
                    "MIDNIGHT_SOURCE_DIFF_SHA256": str(plan["source"]["diff_sha256"]),
                    "MIDNIGHT_SOURCE_TREE_SHA256": str(plan["source"]["uploaded_source_tree_sha256"]),
                },
            )
            sandbox.wait()
            return_code = sandbox.returncode
            stdout = read_stream(sandbox.stdout)
            stderr = read_stream(sandbox.stderr)
            record.update(
                {
                    "returncode": return_code,
                    "stdout": stdout,
                    "stderr_tail": stderr[-8000:],
                    "finished_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
                }
            )
            if stdout:
                try:
                    record["remote_result"] = json.loads(stdout.splitlines()[-1])
                except (json.JSONDecodeError, IndexError):
                    record["remote_result_parse_error"] = "sandbox stdout did not end in one JSON document"
            remote_result = record.get("remote_result")
            remote_manifest = (
                remote_result.get("manifest", {}) if isinstance(remote_result, dict) else {}
            )
            if return_code == 0 and remote_manifest.get("status") == "completed":
                record["measurement_status"] = "completed"
            else:
                record["measurement_status"] = "failed"
                record["measurement_error"] = (
                    record.get("remote_result_parse_error")
                    or remote_manifest.get("failure")
                    or f"worker returned {return_code} or did not complete its campaign"
                )
            results.append(record)
            save_result(run_dir, plan, results)
        except Exception as error:
            record.update(
                {
                    "status": "failed_before_or_during_launch",
                    "measurement_status": "failed",
                    "error_type": type(error).__name__,
                    "measurement_error": str(error),
                    "finished_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
                }
            )
            results.append(record)
            save_result(run_dir, plan, results)
        finally:
            if sandbox is not None:
                try:
                    sandbox.terminate()
                except Exception as error:
                    record["terminate_error"] = f"{type(error).__name__}: {error}"
    return {"tiers": results}


def save_result(run_dir: Path, plan: dict[str, object], tiers: list[dict[str, object]]) -> None:
    status = "running"
    has_failure = any(item.get("measurement_status") == "failed" for item in tiers) or any(
        item.get("status") == "failed_before_or_during_launch" for item in tiers
    )
    if tiers and len(tiers) == len(plan["tiers"]):
        status = (
            "completed_with_failures"
            if has_failure
            else "completed"
        )
    elif has_failure:
        status = "failed"
    value = {
        "schema_version": 1,
        "status": status,
        "created_at_utc": run_dir.name.split("_", 1)[0],
        "host": {"python": sys.version, "platform": platform.platform()},
        "plan": plan,
        "tiers": tiers,
    }
    temporary = run_dir / "result.json.tmp"
    with temporary.open("w", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, run_dir / "result.json")


def main() -> int:
    args = parse_args()
    plan = campaign_plan(args)
    if not plan["within_budget"]:
        print(
            f"Refusing to launch: worst-case estimate ${plan['max_estimated_total_usd']:.2f} exceeds "
            f"configured budget ${args.budget_usd:.2f}.",
            file=sys.stderr,
        )
        return 2
    if args.dry_run:
        print(json.dumps(plan, indent=2))
        return 0

    plan["source"] = source_fingerprint()

    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_id = f"{timestamp}_{args.mode}_{uuid.uuid4().hex[:8]}"
    run_dir = args.output_root / run_id
    run_dir.mkdir(parents=True, exist_ok=False)
    save_result(run_dir, plan, [])
    try:
        result = run_modal(args, plan, run_dir)
    except Exception as error:
        save_result(
            run_dir,
            plan,
            [
                {
                    "status": "failed_before_or_during_launch",
                    "error_type": type(error).__name__,
                    "error": str(error),
                }
            ],
        )
        print(f"Modal campaign failed: {type(error).__name__}: {error}", file=sys.stderr)
        print(f"Run artifact: {run_dir}", file=sys.stderr)
        return 1
    save_result(run_dir, plan, result["tiers"])
    concise_tiers = []
    for item in result["tiers"]:
        summary = item.get("remote_result", {}).get("summary", {})
        concise_tiers.append(
            {
                "tier": item.get("tier", {}).get("name"),
                "measurement_status": item.get("measurement_status", item.get("status")),
                "returncode": item.get("returncode"),
                "worker_screen": [
                    {
                        "workers": row.get("worker_count"),
                        "index_p50_ms": row.get("full_index_wall", {}).get("p50_ms"),
                        "rss_p50_bytes": row.get("resident_highwater", {}).get("p50_bytes"),
                        "catalog_complete": row.get("catalog_complete_all_samples"),
                        "structural_complete": row.get("structural_complete_all_samples"),
                    }
                    for row in summary.get("worker_screen", [])
                ],
                "error": item.get("measurement_error"),
            }
        )
    print(
        json.dumps(
            {
                "run_dir": str(run_dir),
                "status": "completed"
                if all(item.get("measurement_status") == "completed" for item in result["tiers"])
                else "completed_with_failures",
                "max_estimated_total_usd": plan["max_estimated_total_usd"],
                "tiers": concise_tiers,
            },
            indent=2,
        )
    )
    return 0 if all(item.get("measurement_status") == "completed" for item in result["tiers"]) else 2


if __name__ == "__main__":
    raise SystemExit(main())
