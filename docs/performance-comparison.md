# Performance comparison

## Equivalent local before/after trial

The two six-worker samples use the same deterministic fixture (seed `20261103`, 10,000 Rust files, 104,857,600 eligible bytes), the same Intel Mac, Rust 1.99.1 release profile, fresh SQLite database per run, 50 ms process sampling, and warm OS filesystem cache left intact. The only engine change between them was reuse of the `code_files` upsert and `code_symbols` insert prepared statements within each SQLite batch. Each side has five samples; with n=5 the nearest-index p95 and p99 are the observed maximum.

| Metric | Before p50 / p95 / p99 | After p50 / p95 / p99 | Change at p50 |
| --- | ---: | ---: | ---: |
| Full index wall time (ms) | 2,644 / 2,663 / 2,663 | 2,252 / 2,321 / 2,321 | −14.8% |
| Process CPU seconds | 5.069 / 5.104 / 5.104 | 4.687 / 4.929 / 4.929 | −7.5% |
| Files per second | 3,783 / 3,811 / 3,811 | 4,440 / 4,449 / 4,449 | +17.4% |
| MiB per second | 37.8 / 38.1 / 38.1 | 44.4 / 44.5 / 44.5 | +17.4% |
| Direct-child RSS high-water (MB) | 25.94 / 25.99 / 25.99 | 25.89 / 26.25 / 26.25 | +0.2% |
| macOS physical-footprint high-water (MB) | 15.09 / 15.14 / 15.14 | 15.04 / 15.15 / 15.15 | +0.0% |
| Main database + WAL + SHM bytes | 18,722,816 for all samples | 18,722,816 for all samples | no change |

All ten samples completed the file catalog and structural index. Five fixed development queries kept Recall@5 and Recall@10 at 1.0 and MRR at 1.0; Recall@1 remained 0.9 because one query has two equally relevant files. No retrieval ranking or labels changed. This small suite does not substitute for the 64-query held-out suite.

The improvement is concentrated in the SQLite writer: per-file metadata and symbol statements are now prepared once per batch. For one representative six-worker sample, measured metadata writes fell from 1,058 ms to 780 ms and symbol writes from 103 ms to 31 ms. No allocator change, query-ranking change, or indexing architecture change was made.

## Current native macOS fixture results

On the post-change six-worker run (n=5), full indexing was p50/p95/p99 2,252 / 2,321 / 2,321 ms. RSS high-water was 25.89 / 26.25 / 26.25 MB and physical footprint was 15.04 / 15.15 / 15.15 MB. Average CPU use was 2.08 cores at p50. Database main+WAL+SHM size was 18,722,816 bytes. The complete fixture's persistent MCP warm search latency was 12.07 / 12.94 / 13.33 ms (n=25 individual calls across five sessions), while fresh-process startup plus initialize/search was 68.7 / 71.9 / 71.9 ms (n=5). The one-file update visibility p50/p95/p99 was 474 / 482 / 482 ms (n=5), exceeding the proposed 150 ms target.

The 10k-file fixture passed the proposed ≤3,000 ms p95 index, ≤128 MiB peak process memory, ≤15 ms warm retrieval, and ≤250 ms first-context targets on this Intel Mac. The proposed ≤150 ms one-file visibility target did not pass. A full scan still reconciles the repository on each update; no filesystem watcher was added.

## Linux comparison boundary

Exploratory measurements that do not meet the repeated-sample standard are omitted from this paired comparison.

Raw local samples, host state, binary hashes, SQLite sidecar peaks, phase timings, retrieval ranks, and failure records are preserved in the `benchmark/runs/` artifacts.
