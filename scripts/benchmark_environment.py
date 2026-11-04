"""Collect the Linux host fields required on every benchmark result."""

import json
import os
import platform
import re
import subprocess
import tempfile
from pathlib import Path


def _run(args):
    try:
        completed = subprocess.run(args, text=True, capture_output=True, check=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    return completed.stdout.strip()


def environment(binary, path):
    """Return observed host facts and explicit pending statuses, without runtime probes."""
    path = Path(path).resolve()
    cpu_model = None
    try:
        cpu_text = Path("/proc/cpuinfo").read_text()
        match = re.search(r"^model name\s*:\s*(.+)$", cpu_text, re.M)
        cpu_model = match.group(1).strip() if match else None
        system_cores = len(re.findall(r"^processor\s*:", cpu_text, re.M))
    except OSError:
        system_cores = None
    ram_bytes = None
    try:
        match = re.search(r"^MemTotal:\s+(\d+)\s+kB", Path("/proc/meminfo").read_text(), re.M)
        ram_bytes = int(match.group(1)) * 1024 if match else None
    except OSError:
        pass

    with tempfile.TemporaryDirectory(prefix="midnight-runtime-info-") as temp:
        db = Path(temp) / "runtime.sqlite3"
        completed = subprocess.run(
            [str(binary), "--db", str(db), "diagnose"],
            text=True,
            capture_output=True,
            check=True,
        )
        sqlite_version = json.loads(completed.stdout)["sqlite_version"]

    mount = _run(["findmnt", "-n", "-o", "SOURCE,FSTYPE,TARGET", "--target", str(path)])
    devices = _run(["lsblk", "-dn", "-o", "NAME,ROTA,TYPE,TRAN,MODEL"])
    device_limit = None
    if mount and (mount.split()[0] == "overlay" or mount.split()[0].startswith("overlay")):
        device_limit = "filesystem reports overlay; backing physical media is not exposed by this Linux VM"
    return {
        "os": platform.system().lower(),
        "architecture": platform.machine(),
        "cpu_model": cpu_model,
        "logical_core_count_system": system_cores,
        "logical_core_count_available_to_process": os.cpu_count(),
        "ram_bytes": ram_bytes,
        "disk_devices_and_media_fields": devices,
        "filesystem_mount_source_type_target": mount,
        "physical_disk_type_limit": device_limit,
        "kernel": platform.release(),
        "rustc": _run(["rustc", "--version"]),
        "sqlite_version": sqlite_version,
        "out_of_scope_runtimes": {
            "Go": "pending (not run)",
            "Node.js": "pending (not run)",
            "Bun": "pending (not run)",
        },
        "pending_platforms": {
            "macOS reference host": "pending (not run)",
            "Windows": "pending (not run)",
            "ARM": "pending (not run)",
        },
    }
