# Performance and memory investigation

## Scope and evidence

The investigation started on the Intel macOS reference host after Activity Monitor showed `aether-midnight` at about 126% CPU during a repeated Linux Kernel index benchmark. The observation identified active CPU work; it did not measure process memory and did not establish a leak.

The repository was fetched from `origin/main` before work began. The starting commit was `aedadd122ac42f04250900bc64dc607a60e53903`; work is on `perf/midnight-modal-macos-20261104`. The first campaign used the unmodified indexing implementation and five repetitions per worker count. The comparison campaign changed only batched SQLite statement reuse and used the same generated fixture, machine, release profile, database setup, and six-worker setting.

## Resource findings

On the 10,000-file / 100 MiB fixture, the original six-worker run used about 2.02 CPU cores on average during the roughly 2.64-second index. The 126% Activity Monitor reading is therefore consistent with ordinary parallel indexing and is not evidence of memory growth. At 12 workers, throughput was effectively the same as six workers while CPU time and the measured memory high-water marks increased.

The host is macOS 26.7.1, x86_64, Intel Core i5-10400F, 6 physical / 12 logical cores, 24 GiB RAM, APFS on an internal solid-state device. The campaign ran on AC power, with no thermal or performance warning, 73% free memory reported by `memory_pressure`, and about 28 MiB swap used. These are host-wide snapshots, not process-attributed readings.

| Resource | Original six-worker run, n=5 | Statement-reuse run, n=5 | Measurement meaning |
| --- | ---: | ---: | --- |
| Peak RSS p50 / p95 | 25.94 / 25.99 MB | 25.89 / 26.25 MB | Direct-child high-water from `getrusage(RUSAGE_CHILDREN).ru_maxrss` and sampled task RSS, reported separately from footprint |
| macOS physical-footprint high-water p50 / p95 | 15.09 / 15.14 MB | 15.04 / 15.15 MB | `proc_pid_rusage(RUSAGE_INFO_V4)` lifetime maximum; not interchangeable with Linux RSS |
| Average CPU use p50 / p95 | 1.92 / 1.92 cores | 2.08 / 2.12 cores | Process CPU seconds divided by measured wall time |
| Current RSS after warm MCP calls | — | 18.7 MB p50 / 20.9 MB p95 | Sampled within the persistent MCP child; its 2-second idle interval added no observed CPU time |

Decimal MB are used in this table. The RSS high-water and physical footprint are separate operating-system accounting values. The Python collector and filesystem cache are outside the direct-child scope. No allocator live-heap profiler was attached.

### Confirmed memory-flow properties

- Bounded discovery feeds a worker pool and a single SQLite writer. Worker count defaults to available parallelism, capped at 12. Job and result channels have capacity `2 × workers` (minimum 2).
- Each source read is capped at 1 MiB. The owned byte vector is consumed by `String::from_utf8`, so that conversion does not duplicate the source buffer.
- A parser cache is retained by each worker, with at most one parser per supported grammar (Rust, Python, TypeScript, and TSX). Parse trees are local to extraction; the long-lived parser state is bounded by worker and grammar counts.
- The in-flight reservation is three times the source file size, capped at three times the per-file limit. Completed out-of-order results wait in a sequence-keyed map and remain inside the bounded outstanding-item accounting.
- The writer batches at most 512 changed files or 16 MiB of source text by default. Its reported byte counter includes pending source text, identifier terms, and a fixed 128-byte estimate per symbol. It does not include all parser working memory, allocator metadata, SQLite internals, or OS page cache, so it is telemetry rather than an RSS limit.
- SQLite uses WAL mode, normal synchronous writes, and file-backed temporary storage. The 10k fixture's final database plus sidecars was 18,722,816 bytes. The observed WAL high-water varied by sample; the main database and WAL are sampled separately in the raw artifacts.
- The measured six-worker writer phase fell from 1,720 ms to 1,338 ms after prepared-statement reuse. Metadata statement work fell from 1,058 ms to 780 ms, and symbol statement work from 103 ms to 31 ms. FTS execution, parsing, and reads remained material costs.

### Root-cause assessment

| Suspected cause | Finding |
| --- | --- |
| Excessive application heap or a leak | Not supported by this reproduction. Fresh-process full-index samples had a narrow high-water band; persistent MCP samples had no observed CPU during idle. A longer same-process soak with allocator instrumentation remains unrun. |
| Transient parse / writer pressure | Bounded but not fully represented by the byte reservation. macOS physical-footprint high-water stayed near 15 MB for the measured fixture. Large/deep/parser-failure stress fixtures remain pending. |
| Parser state retained by workers | Bounded by worker count and supported grammar count. Per-parser retained allocation is not separately measured. |
| Duplicated text buffers | Source bytes become an owned `String` without a conversion copy. The writer still clones text to assemble FTS payloads; the timer is about 9 ms for this fixture. Removing it is a small possible saving, not a leading measured bottleneck, and was left unchanged. |
| SQLite cache, WAL, or temporary files | WAL grew during indexing and is visible in watched sidecar samples. No runaway sidecar growth was observed; the final summed database size was stable. SQLite cache bytes are not separately exposed by this build. |
| OS file cache | The source tree and temporary database are on local APFS. OS caches were not flushed, so later samples can benefit from filesystem cache. Host-wide `vm_stat`, swap, and memory-pressure snapshots are recorded, but their changes cannot be attributed solely to Midnight. |
| Benchmark harness / subprocess overhead | The legacy runners included Linux-only assumptions and fixed high defaults. The new campaign excludes the Python wrapper from process memory and CPU, separates fresh CLI startup from persistent MCP query latency, and records child scope explicitly. |
| Unexpected parallelism | Worker count is set per run. Six workers produced the best measured throughput plateau on this Mac; 12 workers used more CPU for no useful throughput increase. |
| Measurement defect | Historical Linux `getrusage` peak values were below later sampled RSS and are therefore invalid as peak RSS. The new collector reports sampled RSS, lifetime counters, physical footprint, and cgroup data as distinct fields. |

These findings do not establish behavior on Linux Kernel or VS Code today. The retained repository results report complete file catalogs but incomplete structural indexes (7 and 54 parse-error files, respectively). Those are historical Linux observations, not claims about the new revision.

## Sampling method and limitations

`scripts/measure_process.py` samples one direct child every 50 ms. On macOS it uses `proc_pidinfo(PROC_PIDTASKINFO)` for current resident size, CPU/fault/thread/context-switch counters and `proc_pid_rusage(RUSAGE_INFO_V4)` for physical footprint, lifetime maximum footprint, page-ins, and I/O counters. It also captures host-wide `vm_stat`, swap usage, `memory_pressure`, power, and thermal snapshots before and after each measured child. `/usr/bin/time -l` or Instruments can be used for independent confirmation; `vmmap` is intentionally not spawned repeatedly in the timed path.

On Linux it reads `/proc/<pid>/status`, `/proc/<pid>/stat`, `/proc/<pid>/io`, and periodically `/proc/<pid>/smaps_rollup` for PSS, private, anonymous, file-backed, and swap values. When exposed, it also samples cgroup v2 memory, CPU, I/O, pressure, and event counters. cgroup values cover the sandbox/container and may include the wrapper or other processes, so they are not represented as per-process values.

RSS and physical footprint high-water counters are authoritative OS counters where available; sampled maxima can miss brief peaks. The sample arrays preserve sampled values. The collectors do not aggregate descendants; the measured CLI/MCP commands do not intentionally spawn helper processes. Allocator live bytes, full VM-region classification, and hardware disk write amplification are not measured.

## Artifacts

The machine-readable manifests and raw sample arrays are preserved in the local ignored run directories. For this run, see:

- `benchmark/runs/20261105T002945Z_aedadd122ac4_standard_cc6f7fbc/manifest.json` — pre-change worker-count screen.
- `benchmark/runs/20261105T003600Z_aedadd122ac4_standard_75668a7b/manifest.json` — five-sample before/after trial at six workers, incremental visibility, persistent MCP, and retrieval checks.
- `benchmark/runs/modal/` — immutable Modal campaign attempts and results.

These raw generated artifacts are excluded from Git to avoid committing generated benchmark data. Do not replace historical `benchmark/*.json` evidence with local measurements.
