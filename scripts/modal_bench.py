#!/usr/bin/env python3
"""Run bounded CPU-only AETHER Midnight campaigns on Modal Sandboxes."""

from __future__ import annotations

import argparse
import datetime as dt
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
    worst_case = compute_estimate + args.image_build_reserve_usd
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
        "bash -lc 'source /root/.cargo/env && rustc --version && cargo build --release --locked'",
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
                idle_timeout=30,
                workdir="/opt/midnight",
                block_network=True,
                env={"MIDNIGHT_MODAL_TIER": tier["name"]},
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
    if any(item.get("status") == "failed_before_or_during_launch" for item in tiers):
        status = "failed"
    elif tiers and len(tiers) == len(plan["tiers"]):
        status = "completed" if all(item.get("returncode") == 0 for item in tiers) else "completed_with_failures"
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
    print(json.dumps({"run_dir": str(run_dir), "plan": plan, "tiers": result["tiers"]}, indent=2))
    return 0 if all(item.get("returncode") == 0 for item in result["tiers"]) else 2


if __name__ == "__main__":
    raise SystemExit(main())
