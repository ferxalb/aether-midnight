#!/usr/bin/env python3
"""Measure one child process without third-party dependencies.

The measured scope is the direct child. Midnight's CLI and MCP commands do not
spawn helper processes; the wrapper itself is deliberately excluded. Linux
cgroup readings are reported separately because they include the whole cgroup.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import platform
import resource
import subprocess
import sys
import tempfile
import time
from pathlib import Path


class ProcTaskInfo(ctypes.Structure):
    _fields_ = [
        ("virtual_size", ctypes.c_uint64),
        ("resident_size", ctypes.c_uint64),
        ("total_user", ctypes.c_uint64),
        ("total_system", ctypes.c_uint64),
        ("threads_user", ctypes.c_uint64),
        ("threads_system", ctypes.c_uint64),
        ("policy", ctypes.c_int32),
        ("faults", ctypes.c_int32),
        ("pageins", ctypes.c_int32),
        ("cow_faults", ctypes.c_int32),
        ("messages_sent", ctypes.c_int32),
        ("messages_received", ctypes.c_int32),
        ("syscalls_mach", ctypes.c_int32),
        ("syscalls_unix", ctypes.c_int32),
        ("context_switches", ctypes.c_int32),
        ("thread_count", ctypes.c_int32),
        ("running_threads", ctypes.c_int32),
        ("priority", ctypes.c_int32),
    ]


class RusageInfoV4(ctypes.Structure):
    _fields_ = [
        ("uuid", ctypes.c_uint8 * 16),
        ("user_time", ctypes.c_uint64),
        ("system_time", ctypes.c_uint64),
        ("pkg_idle_wakeups", ctypes.c_uint64),
        ("interrupt_wakeups", ctypes.c_uint64),
        ("pageins", ctypes.c_uint64),
        ("wired_size", ctypes.c_uint64),
        ("resident_size", ctypes.c_uint64),
        ("phys_footprint", ctypes.c_uint64),
        ("proc_start_abstime", ctypes.c_uint64),
        ("proc_exit_abstime", ctypes.c_uint64),
        ("child_user_time", ctypes.c_uint64),
        ("child_system_time", ctypes.c_uint64),
        ("child_pkg_idle_wakeups", ctypes.c_uint64),
        ("child_interrupt_wakeups", ctypes.c_uint64),
        ("child_pageins", ctypes.c_uint64),
        ("child_elapsed_abstime", ctypes.c_uint64),
        ("diskio_bytesread", ctypes.c_uint64),
        ("diskio_byteswritten", ctypes.c_uint64),
        ("cpu_time_qos_default", ctypes.c_uint64),
        ("cpu_time_qos_maintenance", ctypes.c_uint64),
        ("cpu_time_qos_background", ctypes.c_uint64),
        ("cpu_time_qos_utility", ctypes.c_uint64),
        ("cpu_time_qos_legacy", ctypes.c_uint64),
        ("cpu_time_qos_user_initiated", ctypes.c_uint64),
        ("cpu_time_qos_user_interactive", ctypes.c_uint64),
        ("billed_system_time", ctypes.c_uint64),
        ("serviced_system_time", ctypes.c_uint64),
        ("logical_writes", ctypes.c_uint64),
        ("lifetime_max_phys_footprint", ctypes.c_uint64),
        ("instructions", ctypes.c_uint64),
        ("cycles", ctypes.c_uint64),
        ("billed_energy", ctypes.c_uint64),
        ("serviced_energy", ctypes.c_uint64),
        ("interval_max_phys_footprint", ctypes.c_uint64),
        ("runnable_time", ctypes.c_uint64),
    ]


def parse_key_values(text: str, multiplier: int = 1) -> dict[str, int]:
    values = {}
    for line in text.splitlines():
        parts = line.replace(":", " ", 1).split()
        if len(parts) >= 2:
            try:
                values[parts[0]] = int(parts[1]) * multiplier
            except ValueError:
                continue
    return values


def linux_cgroup_dir() -> Path | None:
    try:
        entries = Path("/proc/self/cgroup").read_text().splitlines()
        relative = next(line.split(":", 2)[2] for line in entries if line.startswith("0::"))
    except (OSError, StopIteration):
        return None
    root = Path("/sys/fs/cgroup")
    candidate = root / relative.lstrip("/")
    if candidate.is_dir():
        return candidate
    if root.is_dir():
        return root
    return None


def read_linux_cgroup(path: Path | None) -> dict[str, object] | None:
    if path is None:
        return None
    values: dict[str, object] = {"path": str(path)}
    for filename in ("memory.current", "memory.peak", "memory.max", "cpu.max"):
        try:
            values[filename.replace(".", "_")] = (path / filename).read_text().strip()
        except OSError:
            pass
    for filename in (
        "memory.stat",
        "memory.events",
        "cpu.stat",
        "io.stat",
        "memory.pressure",
        "cpu.pressure",
        "io.pressure",
    ):
        try:
            raw = (path / filename).read_text()
            parsed = {}
            for line in raw.splitlines():
                parts = line.split()
                if len(parts) == 2:
                    try:
                        parsed[parts[0]] = int(parts[1])
                    except ValueError:
                        parsed[parts[0]] = parts[1]
                elif len(parts) > 2:
                    parsed[parts[0]] = {
                        key: value
                        for key, value in (part.split("=", 1) for part in parts[1:] if "=" in part)
                    }
                elif parts:
                    parsed.setdefault("rows", []).append(parts)
            values[filename.replace(".", "_")] = parsed
        except OSError:
            pass
    return values


def read_linux_process(pid: int, elapsed_s: float, rollup: bool) -> dict[str, object]:
    result: dict[str, object] = {"elapsed_s": round(elapsed_s, 6)}
    try:
        status = {}
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            key, _, value = line.partition(":")
            status[key] = value.strip()
        for key in ("VmRSS", "VmHWM", "VmSize", "VmSwap"):
            parts = status.get(key, "").split()
            if parts:
                result["rss_bytes" if key == "VmRSS" else key.lower() + "_bytes"] = int(parts[0]) * 1024
        if "Threads" in status:
            result["thread_count"] = int(status["Threads"])
        for key in ("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"):
            if key in status:
                result[key] = int(status[key])
    except (OSError, ValueError):
        pass

    try:
        stat_text = Path(f"/proc/{pid}/stat").read_text()
        fields = stat_text[stat_text.rfind(")") + 2 :].split()
        ticks = os.sysconf("SC_CLK_TCK")
        result["minor_faults"] = int(fields[7])
        result["major_faults"] = int(fields[9])
        result["cpu_time_ns"] = int((int(fields[11]) + int(fields[12])) * 1_000_000_000 / ticks)
        result["thread_count"] = int(fields[17])
    except (OSError, ValueError, IndexError):
        pass

    try:
        io_values = {}
        for line in Path(f"/proc/{pid}/io").read_text().splitlines():
            key, _, value = line.partition(":")
            io_values[key] = int(value.strip())
        for key in ("read_bytes", "write_bytes", "rchar", "wchar", "syscr", "syscw"):
            if key in io_values:
                result["io_" + key] = io_values[key]
    except (OSError, ValueError):
        pass

    if rollup:
        try:
            rollup_values = parse_key_values(Path(f"/proc/{pid}/smaps_rollup").read_text(), 1024)
            for source, target in (
                ("Rss", "smaps_rss_bytes"),
                ("Pss", "pss_bytes"),
                ("Pss_Anon", "pss_anon_bytes"),
                ("Pss_File", "pss_file_bytes"),
                ("Pss_Shmem", "pss_shmem_bytes"),
                ("Private_Clean", "private_clean_bytes"),
                ("Private_Dirty", "private_dirty_bytes"),
                ("Shared_Clean", "shared_clean_bytes"),
                ("Shared_Dirty", "shared_dirty_bytes"),
                ("Anonymous", "anonymous_bytes"),
                ("Swap", "process_swap_bytes"),
            ):
                if source in rollup_values:
                    result[target] = rollup_values[source]
        except OSError:
            pass
    return result


class MacSampler:
    def __init__(self) -> None:
        self.libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
        self.libproc.proc_pidinfo.argtypes = [
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_uint64,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        self.libproc.proc_pidinfo.restype = ctypes.c_int
        self.libproc.proc_pid_rusage.argtypes = [
            ctypes.c_int,
            ctypes.c_int,
            ctypes.POINTER(RusageInfoV4),
        ]
        self.libproc.proc_pid_rusage.restype = ctypes.c_int

    def sample(self, pid: int, elapsed_s: float) -> dict[str, object]:
        result: dict[str, object] = {"elapsed_s": round(elapsed_s, 6)}
        task = ProcTaskInfo()
        received = self.libproc.proc_pidinfo(
            pid, 4, 0, ctypes.byref(task), ctypes.sizeof(task)
        )
        if received >= ctypes.sizeof(task):
            result.update(
                {
                    "virtual_bytes": int(task.virtual_size),
                    "rss_bytes": int(task.resident_size),
                    "minor_faults": int(task.faults),
                    "page_faults": int(task.faults),
                    "copy_on_write_faults": int(task.cow_faults),
                    "thread_count": int(task.thread_count),
                    "running_threads": int(task.running_threads),
                    "context_switches": int(task.context_switches),
                }
            )
        usage = RusageInfoV4()
        if self.libproc.proc_pid_rusage(pid, 4, ctypes.byref(usage)) == 0:
            result.update(
                {
                    "phys_footprint_bytes": int(usage.phys_footprint),
                    "lifetime_max_phys_footprint_bytes": int(
                        usage.lifetime_max_phys_footprint
                    ),
                    "cpu_time_ns": int(usage.user_time + usage.system_time),
                    "pageins": int(usage.pageins),
                    "disk_read_bytes": int(usage.diskio_bytesread),
                    "disk_write_bytes": int(usage.diskio_byteswritten),
                    "logical_writes": int(usage.logical_writes),
                }
            )
        return result


def command_output(args: list[str], timeout: float = 3.0) -> str | None:
    try:
        completed = subprocess.run(
            args,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return completed.stdout.strip() if completed.returncode == 0 else None


def mac_system_snapshot() -> dict[str, object]:
    result: dict[str, object] = {}
    vm_stat = command_output(["/usr/bin/vm_stat"])
    if vm_stat:
        result["vm_stat"] = vm_stat
    swap = command_output(["/usr/sbin/sysctl", "-n", "vm.swapusage"])
    if swap:
        result["swap_usage"] = swap
    pressure = command_output(["/usr/bin/memory_pressure", "-Q"])
    if pressure:
        result["memory_pressure"] = pressure
    battery = command_output(["/usr/bin/pmset", "-g", "batt"])
    if battery:
        result["power_state"] = battery
    thermal = command_output(["/usr/bin/pmset", "-g", "therm"])
    if thermal:
        result["thermal_state"] = thermal
    return result


def resource_child_maxrss(before, after, system: str) -> int | None:
    value = int(after.ru_maxrss)
    if value <= 0:
        return None
    # macOS reports bytes; Linux and other supported Unix systems report KiB.
    return value if system == "Darwin" else value * 1024


def run_measured(
    command: list[str],
    interval_ms: int,
    stdin_text: str | None,
    stdin_hold_ms: int,
    watched_paths: list[str],
) -> dict[str, object]:
    system = platform.system()
    if system not in ("Darwin", "Linux"):
        raise RuntimeError(f"process telemetry is not implemented for {system}")
    mac_sampler = MacSampler() if system == "Darwin" else None
    cgroup_dir = linux_cgroup_dir() if system == "Linux" else None
    cgroup_before = read_linux_cgroup(cgroup_dir) if system == "Linux" else None
    host_before = mac_system_snapshot() if system == "Darwin" else None
    children_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    samples: list[dict[str, object]] = []
    watched_peaks: dict[str, int] = {}
    started = time.perf_counter()
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        child = subprocess.Popen(
            command,
            stdin=subprocess.PIPE if stdin_text is not None else subprocess.DEVNULL,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        if stdin_text is not None and child.stdin is not None:
            child.stdin.write(stdin_text.encode("utf-8"))
            child.stdin.flush()
            if stdin_hold_ms <= 0:
                child.stdin.close()
        stdin_closed = stdin_text is None or stdin_hold_ms <= 0
        last_rollup = 0.0
        while child.poll() is None:
            elapsed = time.perf_counter() - started
            if not stdin_closed and elapsed * 1000 >= stdin_hold_ms and child.stdin is not None:
                child.stdin.close()
                stdin_closed = True
            if system == "Darwin":
                sample = mac_sampler.sample(child.pid, elapsed)  # type: ignore[union-attr]
            else:
                take_rollup = elapsed - last_rollup >= 0.25
                sample = read_linux_process(child.pid, elapsed, take_rollup)
                if take_rollup:
                    last_rollup = elapsed
            if cgroup_dir is not None:
                cgroup = read_linux_cgroup(cgroup_dir)
                if cgroup is not None:
                    current = cgroup.get("memory_current")
                    if isinstance(current, str) and current.isdigit():
                        sample["cgroup_memory_current_bytes"] = int(current)
                    cpu_stat = cgroup.get("cpu_stat")
                    if isinstance(cpu_stat, dict):
                        sample["cgroup_cpu_usage_usec"] = cpu_stat.get("usage_usec")
                        sample["cgroup_cpu_throttled_usec"] = cpu_stat.get("throttled_usec")
            if watched_paths:
                file_sizes = {}
                for raw_path in watched_paths:
                    path = Path(raw_path)
                    try:
                        size = path.stat().st_size
                    except OSError:
                        size = 0
                    file_sizes[str(path)] = size
                    watched_peaks[str(path)] = max(watched_peaks.get(str(path), 0), size)
                sample["watched_file_sizes_bytes"] = file_sizes
            if len(sample) > 1:
                samples.append(sample)
            time.sleep(max(interval_ms, 1) / 1000)
        returncode = child.wait()
        elapsed_s = time.perf_counter() - started
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read().decode(errors="replace")
        stderr = stderr_file.read().decode(errors="replace")
    children_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    child_maxrss = resource_child_maxrss(children_before, children_after, system)
    cgroup_after = read_linux_cgroup(cgroup_dir) if system == "Linux" else None
    host_after = mac_system_snapshot() if system == "Darwin" else None

    metric_names = (
        "rss_bytes",
        "vmhwm_bytes",
        "phys_footprint_bytes",
        "lifetime_max_phys_footprint_bytes",
        "pss_bytes",
        "pss_anon_bytes",
        "pss_file_bytes",
        "pss_shmem_bytes",
        "anonymous_bytes",
        "private_clean_bytes",
        "private_dirty_bytes",
        "shared_clean_bytes",
        "shared_dirty_bytes",
        "process_swap_bytes",
        "cgroup_memory_current_bytes",
        "cpu_time_ns",
        "minor_faults",
        "page_faults",
        "pageins",
        "thread_count",
        "context_switches",
        "io_read_bytes",
        "io_write_bytes",
        "disk_read_bytes",
        "disk_write_bytes",
        "logical_writes",
    )
    peaks = {}
    for name in metric_names:
        vals = [int(sample[name]) for sample in samples if isinstance(sample.get(name), int)]
        if vals:
            peaks[name] = max(vals)
    cpu_ns = peaks.get("cpu_time_ns")
    if cpu_ns is None:
        cpu_seconds = (children_after.ru_utime - children_before.ru_utime) + (
            children_after.ru_stime - children_before.ru_stime
        )
        cpu_ns = int(cpu_seconds * 1_000_000_000)
        cpu_source = "getrusage(RUSAGE_CHILDREN) delta; direct child only"
    else:
        cpu_source = "sampled process CPU counter; peak counter value"
    maxrss_source = "getrusage(RUSAGE_CHILDREN).ru_maxrss; one isolated direct child"
    process_highwater = child_maxrss
    for name in ("vmhwm_bytes", "lifetime_max_phys_footprint_bytes"):
        if peaks.get(name) is not None:
            process_highwater = max(process_highwater or 0, peaks[name])

    return {
        "command": command,
        "returncode": returncode,
        "elapsed_ms": elapsed_s * 1000,
        "process_scope": "direct child only; wrapper excluded; descendants are not aggregated",
        "sampling_interval_ms": interval_ms,
        "sample_count": len(samples),
        "samples": samples,
        "watched_file_peak_sizes_bytes": watched_peaks,
        "sampled_peaks": peaks,
        "process_highwater_bytes": process_highwater,
        "ru_maxrss_bytes": child_maxrss,
        "ru_maxrss_status": maxrss_source,
        "cpu_seconds": cpu_ns / 1_000_000_000,
        "cpu_measurement": cpu_source,
        "stderr_tail": stderr[-4000:],
        "stdout": stdout,
        "cgroup_before": cgroup_before,
        "cgroup_after": cgroup_after,
        "mac_host_before": host_before,
        "mac_host_after": host_after,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interval-ms", type=int, default=50)
    parser.add_argument("--stdin-text")
    parser.add_argument("--stdin-hold-ms", type=int, default=0)
    parser.add_argument("--watch-path", action="append", default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("provide a command after --")
    if args.interval_ms <= 0:
        parser.error("--interval-ms must be positive")
    if args.stdin_hold_ms < 0:
        parser.error("--stdin-hold-ms cannot be negative")
    try:
        measurement = run_measured(
            command, args.interval_ms, args.stdin_text, args.stdin_hold_ms, args.watch_path
        )
    except Exception as error:
        json.dump({"measurement_error": f"{type(error).__name__}: {error}"}, sys.stdout)
        print()
        return 2
    json.dump(measurement, sys.stdout)
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
