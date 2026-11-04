# Performance contract and benchmark results

The numbers below are measurements from the release build on the documented local fixture and host. They are not latency guarantees for other machines or repositories. The V1 targets remain unchanged; a failed result is reported as a failure.

## Reproduce

From the repository root, run:

```sh
python3 scripts/run_benchmark.py > benchmark/results.json
```

The runner builds with `cargo build --release --locked`, generates a temporary corpus from `benchmark/manifest.json`, runs the engine benchmark and an FTS5 tokenizer comparison on that same fixture, then writes one JSON object to stdout. Generator progress/build messages go to stderr. The checked-in [results JSON](../benchmark/results.json) is the measured run summarized here. The fixture directory is removed when the runner exits; only the manifest and deterministic generator are retained.

Defaults are 30 search/first-result samples, 3 fresh-database full-index samples, and 20 stress cycles. Override counts with `MIDNIGHT_BENCH_SAMPLES`, `MIDNIGHT_INDEX_SAMPLES`, and `MIDNIGHT_STRESS_CYCLES`. The fixture generator refuses to overwrite an existing output directory. Its fixed manifest seed is `20261103`; it produces exactly 10,000 eligible Rust text files totaling 104,857,600 bytes. No corpus files were skipped.

The timer for first useful context starts immediately before launching a fresh Midnight process with an empty index and ends when grounded JSON is returned. The build is excluded. Full-index samples use new databases. Warm-search timing is measured inside the benchmark process against the complete index and includes current-file validation. Incremental timing starts after writing one changed file and ends after indexing plus a fresh query sees the update. OS file-cache eviction is not attempted: the first read is cache-state unknown and later runs can be warm. The concurrent partial-index check is one fresh-process sample while a separate full-corpus index process is still running.

Percentiles use the runner's sorted, nearest-rank-style estimator `ceil((n - 1) × p)`, with the index capped at `n - 1`. Full-index p95 has only three samples, and concurrent partial search has one sample.

## Gate results

| v0.1 gate | Target | Measured p50 / p95 (samples) | Result |
| --- | ---: | ---: | --- |
| First useful grounded context, fresh process and empty index | p50 ≤ 1,000 ms; p95 ≤ 3,000 ms | 156.7 / 167.2 ms (30) | Pass |
| Complete lexical and structural index, 10,000 files / 100 MiB | p95 ≤ 3,000 ms | 4,722.1 / 7,142.1 ms (3) | **Fail** |
| Useful partial result while full indexing continues | ≤ 3,000 ms | 150.6 ms (1); index process was active and result marked partial | Pass on one sample |
| Warm search on a complete index | p95 ≤ 50 ms | 15.4 / 16.4 ms (30) | Pass |
| One-file incremental update, query visible | p95 ≤ 1,000 ms | 315.5 / 424.5 ms (10) | Pass |

The full-index target remains the release-gate miss after the tokenizer change. In the first measured sample of this run, file discovery took 31 ms, reading/hashing 3,389 ms, Tree-sitter parsing 938 ms, index writes 1,771 ms, and commit/reconciliation 436 ms; the reported `full_index_ready` phase was 7,123 ms. Reading and hashing is now the largest phase, with FTS/index writing second. The target has not been relaxed.

## Fixture, retrieval, and resources

| Measurement | Result |
| --- | ---: |
| Eligible files / source bytes | 10,000 / 104,857,600 |
| Skipped files / reasons | 0 / none |
| SQLite database plus WAL/SHM bytes after the benchmark's update workload | 18,710,528 |
| Index-to-source byte ratio | 0.178× |
| Retrieval-quality queries | 5 fixed synthetic cases |
| Recall@5 / MRR | 1.00 / 1.00 |
| Memory temporal, provenance, forget, and scope cases | All true |

The five labelled code cases are exact symbol, path, lexical, cross-file symbol, and natural-language. Perfect retrieval scores apply only to this small deterministic fixture, not to arbitrary code queries. Memory checks cover temporal active/expired selection, a source link, namespace isolation, and immediate forget suppression.

The locally run `rg 15.2.0` baseline scanned the same files for the same five fixed queries: p50 122.5 ms, p95 130.7 ms, sample count 5. Midnight warm search was p50 15.4 ms and p95 16.4 ms, sample count 30. These timings have different boundaries: each `rg` subprocess includes process launch and full scan; Midnight uses its in-process search function against a warm index and verifies current-source evidence. This is a measured local comparison, not a universal engine ranking. Tantivy and Zoekt were not installed and were not evaluated or shipped.

## FTS5 tokenizer evaluation

The runner builds contentless FTS5 indexes for the four fixed content queries (exact symbol, lexical, cross-file symbol, and natural language), using the same generated source files. It times 30 warm searches per query and measures the exact path query in a separate indexed path table. The comparator uses the host Python SQLite 3.51.0; Midnight's release build uses bundled SQLite 3.54.1. Each comparison index uses 512-file batches and includes file reads, FTS writes, and a WAL checkpoint.

| Tokenizer | Build time | FTS index bytes | Ratio to source | Warm search p50 / p95 (120 samples) | Recall@5 / MRR |
| --- | ---: | ---: | ---: | ---: | ---: |
| FTS5 trigram | 6,337 ms | 112,640,000 | 1.074× | 2.09 / 2.90 ms | 1.00 / 1.00 |
| FTS5 `unicode61 remove_diacritics 2` | 1,504 ms | 15,368,192 | 0.147× | 0.38 / 0.62 ms | 1.00 / 1.00 |

Both tokenizers returned all four relevant content-query sets in the top five on this fixture. The separate exact path lookup measured p50 0.0025 ms / p95 0.0037 ms (30 samples). Midnight now uses `unicode61`, which supports short tokens and substantially reduces index size in this experiment. It matches whole tokens; arbitrary mid-token substring search is not part of this tokenizer's behavior. The four-query synthetic result does not establish quality on general repositories.

The benchmark process peak RSS was 28,917,760 bytes. Across 20 repeated open/index/update/search/status/close cycles, current RSS moved from 28,827,648 to 28,884,992 bytes, a 57,344-byte increase. Cancellation remained partial. Allocator live-heap and mmap-page measurements are `null` because this build does not instrument them. Child-process RSS is not included in the benchmark process's high-water mark. These RSS observations do not establish zero leaks.

## Reference environment

- macOS 26.7.1
- APFS data volume reported as 240 GiB, with 49 GiB available during the final run.
- Rust 1.99.1; release profile with thin LTO and one codegen unit.
- `rusqlite` 0.41.1 with bundled SQLite 3.54.1; Tree-sitter 0.27.1, Rust grammar 0.24.2, Python grammar 0.25.0, TypeScript grammar 0.23.2.
- ripgrep 15.2.0, Node.js 26.13.0, Go 1.27.3. Bun was not installed.
