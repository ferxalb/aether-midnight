#!/usr/bin/env python3
"""Generate the fixed 10,000-file / 100 MiB Midnight benchmark corpus."""

import argparse
import json
from pathlib import Path

MANIFEST = Path(__file__).resolve().parents[1] / "benchmark" / "manifest.json"


def build_file(index: int, size: int, seed: int) -> bytes:
    lines = [f"pub fn filler_{index:05d}() -> usize {{ {index} }}"]
    if index == 0:
        lines.append("pub fn benchmark_target_00000() -> usize { 0 }")
    if index == 4 or index == 5:
        lines.append("pub fn common_retry_budget() -> usize { 3 }")
    lines.append(f"// fixture_path=src/{index:05d}.rs")
    if index == 2:
        lines.append("// delta cache invalidation token")
    if index == 3:
        lines.append("// stale revision consistency")
    prefix = ("\n".join(lines) + "\n").encode("ascii")
    payload_prefix = prefix + b"// payload "
    if len(payload_prefix) >= size:
        raise ValueError(f"fixture prefix exceeds file target ({index})")
    pattern = f"seed{seed:08d}-file{index:05d}-".encode("ascii")
    padding_size = size - len(payload_prefix) - 1
    repeated = (pattern * ((padding_size + len(pattern) - 1) // len(pattern)))[:padding_size]
    content = payload_prefix + repeated + b"\n"
    if len(content) != size:
        raise AssertionError((index, len(content), size))
    return content


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    spec = json.loads(MANIFEST.read_text(encoding="utf-8"))
    fixture = spec["fixture"]
    count = fixture["eligible_files"]
    total_bytes = fixture["eligible_bytes"]
    seed = fixture["seed"]
    if args.output.exists():
        raise FileExistsError(f"refusing to overwrite existing fixture directory: {args.output}")
    source_dir = args.output / "src"
    source_dir.mkdir(parents=True)
    base, remainder = divmod(total_bytes, count)
    for index in range(count):
        size = base + (1 if index < remainder else 0)
        path = source_dir / f"{index:05d}.rs"
        path.write_bytes(build_file(index, size, seed))
    actual_files = sum(1 for path in args.output.rglob("*") if path.is_file())
    actual_bytes = sum(path.stat().st_size for path in args.output.rglob("*") if path.is_file())
    if (actual_files, actual_bytes) != (count, total_bytes):
        raise AssertionError((actual_files, actual_bytes, count, total_bytes))
    print(json.dumps({"root": str(args.output.resolve()), "eligible_files": actual_files, "eligible_bytes": actual_bytes, "seed": seed}))


if __name__ == "__main__":
    main()
