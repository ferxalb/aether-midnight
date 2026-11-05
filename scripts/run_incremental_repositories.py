#!/usr/bin/env python3
"""Measure incremental update visibility in local real repositories."""

import argparse
import json
import math
import os
import platform
import re
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def distribution(values, unit="ms"):
    ordered = sorted(values)
    def pct(p):
        return ordered[min(math.ceil((len(ordered) - 1) * p), len(ordered) - 1)]
    return {"n":len(ordered),f"p50_{unit}":pct(.50),f"p95_{unit}":pct(.95),f"p99_{unit}":pct(.99),f"samples_{unit}":ordered}


def environment(binary):
    if platform.system() == "Darwin":
        cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], text=True, capture_output=True, check=True).stdout.strip()
        ram = int(subprocess.run(["sysctl", "-n", "hw.memsize"], text=True, capture_output=True, check=True).stdout.strip())
        disk = subprocess.run(["diskutil", "info", "/"], text=True, capture_output=True, check=True).stdout
        fs = next((line.strip() for line in disk.splitlines() if "File System Personality:" in line), None)
        fs = fs or "unavailable from diskutil info /"
        kernel = platform.release()
    else:
        cpu = subprocess.run(["lscpu","-p=MODELNAME"],text=True,capture_output=True,check=True).stdout
        cpu = next((line for line in cpu.splitlines() if line and not line.startswith("#")),None)
        ram = int(re.search(r"^MemTotal:\s+(\d+)",Path("/proc/meminfo").read_text(),re.M).group(1))*1024
        disk = subprocess.run(["lsblk","-dn","-o","NAME,ROTA,TYPE,TRAN,MODEL"],text=True,capture_output=True).stdout.strip()
        fs = subprocess.run(["findmnt","-n","-o","SOURCE,FSTYPE,TARGET","--target",str(ROOT)],text=True,capture_output=True).stdout.strip()
        kernel = os.uname().release
    sqlite_db = tempfile.NamedTemporaryFile(delete=False).name
    try:
        diag = json.loads(subprocess.run([str(binary),"--db",sqlite_db,"diagnose"],text=True,capture_output=True,check=True).stdout)
    finally:
        Path(sqlite_db).unlink(missing_ok=True)
    storage_limit = ("diskutil reports an internal solid-state APFS data volume; exact SSD model is unavailable"
                     if platform.system() == "Darwin"
                     else "VM reports virtio block device; physical backing media cannot be identified")
    return {"os":platform.system().lower(),"arch":platform.machine(),"cpu_model":cpu,"logical_cores":os.cpu_count(),"ram_bytes":ram,
        "disk_type_report":disk,"filesystem_mount":fs,"kernel":kernel,
        "rustc":subprocess.run(["rustc","--version"],text=True,capture_output=True,check=True).stdout.strip(),
        "sqlite_version":diag["sqlite_version"],"storage_media_limit":storage_limit}


def db_size(db):
    return sum(path.stat().st_size for path in (db,Path(f"{db}-wal"),Path(f"{db}-shm")) if path.exists())


def run(binary,db,*args):
    return [str(binary),"--db",str(db),*map(str,args)]


def incremental(binary, repo, name, relative_path, count):
    file = repo / relative_path
    original = file.read_bytes()
    db = Path(tempfile.mkdtemp(prefix=f"midnight-incremental-{name}-")) / "index.sqlite3"
    results=[]
    durations=[]
    catalog=subprocess.run(run(binary,db,"index","--repo",repo),text=True,capture_output=True)
    if catalog.returncode:
        raise RuntimeError(f"initial {name} index exited {catalog.returncode}: {catalog.stderr[-3000:]}")
    status=json.loads(catalog.stdout)
    if not status.get("catalog_complete",status.get("complete",False)):
        raise RuntimeError(f"initial {name} catalog did not complete")
    db_bytes_before=db_size(db)
    current=original
    clean_status=subprocess.run(["git","-C",str(repo),"rev-parse","HEAD"],text=True,capture_output=True,check=True).stdout.strip()
    try:
        for i in range(count):
            marker=f"midnight_visibility_token_{i:05d}"
            suffix=("\n// "+marker+"\n").encode()
            current += suffix
            file.write_bytes(current)
            start=time.perf_counter()
            indexed=subprocess.run(run(binary,db,"index","--repo",repo),text=True,capture_output=True)
            if indexed.returncode:
                raise RuntimeError(f"incremental index failed for {name}: {indexed.stderr[-1000:]}")
            index_status=json.loads(indexed.stdout)
            if not index_status.get("catalog_complete",index_status.get("complete",False)):
                raise RuntimeError(f"incremental catalog incomplete for {name}")
            searched=subprocess.run(run(binary,db,"search","--repo",repo,"--query",marker,"--limit","5"),text=True,capture_output=True)
            if searched.returncode:
                raise RuntimeError(f"visibility search failed for {name}: {searched.stderr[-1000:]}")
            value=json.loads(searched.stdout)
            visible=any(row.get("path")==relative_path and row.get("freshness")=="fresh" for row in value.get("results",[]))
            elapsed=(time.perf_counter()-start)*1000
            if not visible:
                raise RuntimeError(f"incremental marker {marker} was not fresh and query-visible")
            durations.append(elapsed)
            results.append({"sample":i,"elapsed_ms":elapsed,"marker":marker,"visible":visible,
                "catalog_complete":index_status.get("catalog_complete"),"structural_complete":index_status.get("structural_complete"),
                "database_bytes_after_sample":db_size(db)})
    finally:
        file.write_bytes(original)
        restored=subprocess.run(run(binary,db,"index","--repo",repo),text=True,capture_output=True)
        if restored.returncode:
            raise RuntimeError(f"failed to restore indexed repository {name}: {restored.stderr[-1000:]}")
    status_git=subprocess.run(["git","-C",str(repo),"rev-parse","HEAD"],text=True,capture_output=True,check=True).stdout.strip()
    if status_git!=clean_status:
        raise RuntimeError(f"repository commit changed during incremental update run for {name}")
    dirty=subprocess.run(["git","-C",str(repo),"status","--porcelain","--",relative_path],text=True,capture_output=True,check=True).stdout.strip()
    if dirty:
        raise RuntimeError(f"temporary edit was not restored for {name}: {dirty}")
    sizes=[sample["database_bytes_after_sample"] for sample in results]
    growth=[size-db_bytes_before for size in sizes]
    return {"target_file":relative_path,"sample_count":len(results),"timing":distribution(durations),
        "catalog_complete_all_samples":all(row.get("catalog_complete") for row in results),
        "structural_complete_all_samples":all(row.get("structural_complete") for row in results),
        "structural_index_status":("complete" if results and all(row.get("structural_complete") for row in results)
            else "catalog_only_incomplete"),
        "database_bytes_during_updates":distribution(sizes,"bytes"),
        "database_growth_bytes_during_updates":distribution(growth,"bytes"),
        "database_bytes_before_updates":db_bytes_before,"database_bytes_after_updates":db_size(db),
        "database_bytes_before_updates_evidence":{**distribution([db_bytes_before],"bytes"),
            "status":"anecdotal: one database-size snapshot"},
        "database_bytes_after_updates_evidence":{**distribution([db_size(db)],"bytes"),
            "status":"anecdotal: one database-size snapshot"},
        "file_restoration_check":{"n":1,"status":"anecdotal: one post-run Git cleanliness check","restored_and_clean":True},
        "samples":results,"file_restored_and_clean":True,"cache_condition":"warm complete DB; OS cache not evicted"}


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--repo",action="append",required=True,metavar="NAME=PATH@RELATIVE_FILE")
    parser.add_argument("--binary",type=Path,default=ROOT/"target/release/aether-midnight")
    parser.add_argument("--samples",type=int,default=5)
    args=parser.parse_args()
    if not 1 <= args.samples <= 100:
        parser.error("--samples must be between 1 and 100")
    binary=args.binary.resolve()
    report={"schema_version":1,"environment":environment(binary),"sample_count_per_repository":args.samples,
        "timing_boundary":"after file write, before index process start, through fresh CLI search confirming the updated file is fresh and query-visible",
        "cache_condition":"fresh index setup; OS cache left untouched; update/search use a warm complete index",
        "repositories":[]}
    for spec in args.repo:
        name,rest=spec.split("=",1); path,target=rest.rsplit("@",1); repo=Path(path).resolve()
        url=subprocess.run(["git","-C",str(repo),"remote","get-url","origin"],text=True,capture_output=True,check=True).stdout.strip()
        commit=subprocess.run(["git","-C",str(repo),"rev-parse","HEAD"],text=True,capture_output=True,check=True).stdout.strip()
        result=incremental(binary,repo,name,target,args.samples)
        report["repositories"].append({"name":name,"url":url,"commit":commit,"path":str(repo),**result})
        print(f"completed incremental visibility for {name}: n={result['sample_count']}",file=__import__("sys").stderr)
    json.dump(report,__import__("sys").stdout,indent=2)
    print()


if __name__=="__main__":
    main()
