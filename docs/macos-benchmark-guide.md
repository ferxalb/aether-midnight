# Native macOS benchmark guide

The local campaign uses a direct-child sampler rather than Activity Monitor's instantaneous CPU display. It records sampled RSS and the process high-water counter separately from macOS physical footprint, CPU time, page-ins, I/O, faults, threads, and context switches. Host snapshots record `vm_stat`, swap, `memory_pressure`, power, and thermal state. The Linux collector separately reports PSS, anonymous/file-backed memory, cgroup limits/events, pressure, and I/O when available.

## Reproduce the reference fixture

Run from the repository root on the Mac being measured. The smoke run creates a new timestamped artifact directory and a temporary 10k-file / 100 MiB source fixture. It does not modify a caller-owned repository or overwrite a committed result.

```sh
rustc --version
cargo --version
sysctl -n machdep.cpu.brand_string
sysctl -n hw.physicalcpu
sysctl -n hw.logicalcpu
sysctl -n hw.memsize
sysctl -n machdep.cpu.features
diskutil info -plist / | plutil -p -
pmset -g therm
memory_pressure -Q
cargo build --release --locked
python3 scripts/performance_campaign.py --mode smoke --workers 1,2,4,6 \
  --update-workers 4 --interval-ms 50 --max-duration-s 600
```

The smoke mode uses one sample per measurement. For a standard run, use five samples and a longer bounded window:

```sh
python3 scripts/performance_campaign.py --mode standard --workers 1,2,4,6,12 \
  --update-workers 4 --interval-ms 50 --max-duration-s 1800
```

For one controlled optimization comparison, keep the same binary configuration except for the intended source change, workload seed, selected worker count, output code version, power conditions, and cache condition. Save each output directory; never direct it to `benchmark/results.json`.

## Reference observations

The Intel reference machine used for the November 2026 run was x86_64, 6 physical / 12 logical CPU cores, Intel Core i5-10400F, 24 GiB RAM, APFS on an internal SSD, AC power, and no thermal warning. The runner recorded the advertised CPU feature strings. No Rosetta translation or Apple Silicon result is claimed. Apple Silicon requires a native arm64 build and a separate run on actual arm64 hardware.

On this Mac, six workers formed the best measured throughput point for the 10k/100 MiB fixture; twelve workers did not improve throughput. Use these results as a device-specific observation, not as a Linux or Apple Silicon prediction. APFS OS file cache was left intact. Creating a new database does not make the source fixture's OS pages cold.

For an independent one-off inspection, `/usr/bin/time -l target/release/aether-midnight ...` can provide resource-usage counters, `vmmap <pid>` can inspect VM regions, and Instruments can record allocator and CPU profiles. Keep diagnostic tools out of the timed process path and label their results separately. The built-in collector samples the direct child at 50 ms; sampled peaks can miss brief events, while `ru_maxrss` and `RUSAGE_INFO_V4` lifetime values are retained as separate OS high-water counters.

## Reproduction limits

The reference campaign contains five-sample full-index screens at 1/2/4/6/12 workers before the prepared-statement optimization and a five-sample six-worker after run. It also includes persistent MCP searches, idle CPU, one-file update visibility, and a five-query development retrieval suite. It does not establish Apple Silicon performance, Linux Kernel or VS Code status on the current revision, a 100k memory-record benchmark, filesystem-event watcher behavior, OS-cache-cold performance, thermal throttling over hours, or allocator-live-heap totals.
