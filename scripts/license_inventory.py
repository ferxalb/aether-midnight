#!/usr/bin/env python3
"""Emit the lockfile-backed dependency/license inventory as JSON."""

import argparse
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "THIRD_PARTY_LICENSES.json",
        help="output path (refuses to replace an existing file)",
    )
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(f"refusing to overwrite existing inventory: {args.output}")
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    root_id = metadata["resolve"]["root"]
    root_node = next(node for node in metadata["resolve"]["nodes"] if node["id"] == root_id)
    direct_ids = {dependency["pkg"] for dependency in root_node["deps"]}
    entries = []
    for package in metadata["packages"]:
        if package["source"] is None:
            continue
        entries.append(
            {
                "name": package["name"],
                "version": package["version"],
                "license": package["license"] or "UNDECLARED",
                "license_file": package["license_file"],
                "source": package["source"],
                "direct_dependency": package["id"] in direct_ids,
            }
        )
    entries.sort(key=lambda item: (item["name"], item["version"]))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "generated_from": "Cargo.lock via cargo metadata --locked",
                "package_count": len(entries),
                "packages": entries,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"wrote {len(entries)} packages to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
