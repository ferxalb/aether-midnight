"""Collect platform-aware host fields for the legacy benchmark commands."""

from __future__ import annotations

import json
import os
import platform
import plistlib
import re
import subprocess
import tempfile
from pathlib import Path


def _run(args: list[str], timeout: float = 5.0) -> str | None:
    try:
        completed = subprocess.run(
            args, text=True, capture_output=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return completed.stdout.strip() if completed.returncode == 0 else None


def _sysctl(name: str) -> str | None:
    return _run(["/usr/sbin/sysctl", "-n", name])


def _linux_facts(path: Path) -> dict[str, object]:
    cpu_model = None
    logical_cores = os.cpu_count()
    ram_bytes = None
    try:
        cpu_text = Path("/proc/cpuinfo").read_text()
        match = re.search(r"^model name\s*:\s*(.+)$", cpu_text, re.M)
        cpu_model = match.group(1).strip() if match else None
        logical_cores = len(re.findall(r"^processor\s*:", cpu_text, re.M)) or logical_cores
    except OSError:
        pass
    try:
        match = re.search(r"^MemTotal:\s+(\d+)\s+kB", Path("/proc/meminfo").read_text(), re.M)
        ram_bytes = int(match.group(1)) * 1024 if match else None
    except OSError:
        pass
    return {
        "cpu_model": cpu_model,
        "logical_core_count_system": logical_cores,
        "physical_core_count_system": None,
        "ram_bytes": ram_bytes,
        "disk_devices_and_media_fields": _run(
            ["lsblk", "-dn", "-o", "NAME,ROTA,TYPE,TRAN,MODEL"]
        ),
        "filesystem_mount_source_type_target": _run(
            ["findmnt", "-n", "-o", "SOURCE,FSTYPE,TARGET", "--target", str(path)]
        ),
        "physical_disk_type_limit": "Linux virtual/overlay storage may hide the backing device.",
        "kernel": platform.release(),
    }


def _macos_facts(path: Path) -> dict[str, object]:
    cpu = _sysctl("machdep.cpu.brand_string") or _sysctl("hw.model")
    raw_memory = _sysctl("hw.memsize")
    disk_fields = None
    filesystem = None
    try:
        raw = subprocess.run(
            ["/usr/sbin/diskutil", "info", "-plist", str(path)],
            capture_output=True,
            timeout=5,
            check=True,
        ).stdout
        info = plistlib.loads(raw)
        disk_fields = {
            key: info.get(key)
            for key in ("DeviceIdentifier", "DeviceLocation", "SolidState", "BusProtocol", "MediaName")
            if info.get(key) is not None
        }
        filesystem = info.get("FilesystemType")
    except (OSError, subprocess.SubprocessError, ValueError):
        pass
    if not filesystem:
        text = _run(["/usr/sbin/diskutil", "info", str(path)])
        if text:
            filesystem = next(
                (
                    line.partition(":")[2].strip()
                    for line in text.splitlines()
                    if "File System Personality:" in line
                ),
                None,
            )
    return {
        "cpu_model": cpu,
        "logical_core_count_system": int(_sysctl("hw.logicalcpu") or 0) or os.cpu_count(),
        "physical_core_count_system": int(_sysctl("hw.physicalcpu") or 0) or None,
        "ram_bytes": int(raw_memory) if raw_memory and raw_memory.isdigit() else None,
        "disk_devices_and_media_fields": disk_fields,
        "filesystem_mount_source_type_target": filesystem,
        "physical_disk_type_limit": "diskutil metadata; exact physical SSD model can be unavailable.",
        "kernel": platform.release(),
    }


def environment(binary: str | Path, path: str | Path | None = None) -> dict[str, object]:
    """Return observed host and bundled SQLite facts without runtime probes."""
    binary = str(Path(binary).resolve())
    target = Path(path or Path.cwd()).resolve()
    system = platform.system()
    if system == "Darwin":
        facts = _macos_facts(target)
    elif system == "Linux":
        facts = _linux_facts(target)
    else:
        facts = {
            "cpu_model": platform.processor() or None,
            "logical_core_count_system": os.cpu_count(),
            "physical_core_count_system": None,
            "ram_bytes": None,
            "disk_devices_and_media_fields": None,
            "filesystem_mount_source_type_target": None,
            "physical_disk_type_limit": f"resource collector not implemented for {system}",
            "kernel": platform.release(),
        }

    with tempfile.TemporaryDirectory(prefix="midnight-runtime-info-") as temporary:
        database = Path(temporary) / "runtime.sqlite3"
        completed = subprocess.run(
            [binary, "--db", str(database), "diagnose"],
            text=True,
            capture_output=True,
            timeout=10,
            check=True,
        )
        sqlite_version = json.loads(completed.stdout)["sqlite_version"]

    return {
        "os": system.lower(),
        "architecture": platform.machine(),
        "os_version": platform.platform(),
        **facts,
        "logical_core_count_available_to_process": os.cpu_count(),
        "rustc": _run(["rustc", "--version"]),
        "sqlite_version": sqlite_version,
        "python_sqlite_version": __import__("sqlite3").sqlite_version,
        "out_of_scope_runtimes": {
            "Go": "pending (not run)",
            "Node.js": "pending (not run)",
            "Bun": "pending (not run)",
        },
        "pending_platforms": {
            "Windows": "pending (not run)",
            "Apple Silicon": "pending (not run)",
        },
    }
