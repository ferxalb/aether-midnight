# v0.5 exact recall: descending FTS scan

> Historical experiment: the production default introduced here is superseded by [the v0.5 correction](retrieval-v05-correction.md), which restores the original direct scan after paired regression diagnosis. Measurements below remain unchanged.

This is a bounded improvement on `767dbba`, not completion of the v0.5 latency goals. Work stayed on the existing `main` branch at the operator's request; pushing that branch was explicitly authorized. Native macOS was available. Raw results and experimental programs remain in ignored local storage; only workload definitions and aggregate measurements are published here.

## Confirmed cost and implementation

Broad exact recall still scores every matching document. A second avoidable cost appears when many rows tie: the ascending FTS row-ID traversal presents successively better tie-break IDs to SQLite's top-k sorter. Every new equal-score row replaces a heap entry because the required tie-break is newest ID first.

For single-token queries only, `MEMORY_RANKED_DESC_SQL` feeds the exact sorter from a descending row-ID coroutine. `LIMIT -1` prevents query flattening without removing any match. The outer order remains `(rank, rowid DESC)`; only its input order changes. Winners still pass the original namespace, deletion, validity and Rust coverage checks, with widening when necessary. Query-plan diagnostics select the same statement as production. Multi-term stages retain the old statement.

There is no new index, migration, dependency, background task, cache invalidation policy, or write-path change. FTS5 BM25, global corpus statistics, class staging, provenance, presentation order, honest abstention and completeness labels are preserved. This reduces sorting work, not BM25's O(matches) cost.

## Experiments and tradeoffs

- Applying the coroutine to every stage made multi-term queries slower in the exploratory 100k one-namespace run: two heavy terms p50 14.95 → 16.04 ms; heavy-plus-rare 2.79 → 3.16 ms. That version was rejected. The final change selects it only for single-token queries.
- A bounded offline auxiliary-postings prototype groups single-term candidates by term frequency, then reads the shortest documents in each group before an exact BM25 merge. It tests top-10 agreement with FTS5 on an ASCII synthetic corpus, not class-staged, temporal or semantic quality. Its SQLite runtime is separate from the bundled engine. It is not a production implementation and has no migration/recovery protocol. Measurements below describe only that prototype.
- Recency sampling remains opt-in and explicitly approximate. Morphology and bilingual expansion were not introduced: neither new independent relevance labels nor a contract-preserving quality improvement was established in this work. Existing accent normalization and bilingual stop words remain available.

## Native paired measurements

Baseline was rebuilt from `767dbba`; the implementation binary was preserved separately. Final runs were sequential on the same native Intel macOS host, with no compilation during measurements. This is a warm, shared-host observation, not an exclusive-host or cold-cache claim. Earlier exploratory runs that overlapped compilation are excluded.

`bench-memory`: 100 persistent recall samples per size/layout, limit 10, ubiquitous term and tied documents. Values are p50/p95/p99 milliseconds. CPU includes setup and all operations for that size; RSS is process high-water and cumulative across sizes. These are not per-query CPU/RSS readings.

| Records / namespaces | Baseline recall | New recall | Workload CPU seconds old → new | Peak RSS MiB old → new | DB bytes (both) |
| --- | --- | --- | --- | --- | --- |
| 10,000 / 1 | 7.06/7.21/8.25 | 5.64/6.17/6.77 | 2.03 → 1.76 | 17.64 → 17.93 | 2,711,552 |
| 100,000 / 1 | 71.23/74.82/122.61 | 54.76/55.35/55.57 | 18.82 → 14.93 | 26.98 → 28.15 | 26,001,408 |
| 10,000 / 4 | 2.65/2.76/2.91 | 2.38/2.45/2.64 | 1.29 → 1.28 | 17.96 → 18.55 | 2,695,168 |
| 100,000 / 4 | 24.62/25.01/25.13 | 21.70/23.22/56.08 | 9.18 → 8.80 | 29.97 → 30.55 | 25,563,136 |

`bench-retrieval`: 100k Zipf documents, 50 samples per query/strategy, limit 10. The table compares the staged strategy between actual binaries; retained v0.3.1 and opt-in recency strategies are not the baseline here. All classes are shown, including slower ones. Values are p50/p95/p99 milliseconds; CPU is mean milliseconds/query. Unchanged multi-term SQL also shows run-to-run tail variation.

| Namespaces | Class | Baseline | New | CPU old → new |
| --- | --- | --- | --- | --- |
| 1 | broad_ubiquitous_single_term | 53.22/53.98/54.16 | 56.62/57.79/58.07 | 53.17 → 56.58 |
| 1 | broad_mid_single_term | 12.85/13.35/13.46 | 13.53/13.87/14.30 | 12.90 → 13.56 |
| 1 | selective_single_term | 0.37/0.44/0.44 | 0.38/0.44/0.46 | 0.38 → 0.39 |
| 1 | two_heavy_terms | 15.38/16.24/16.28 | 15.10/15.77/16.28 | 15.38 → 15.17 |
| 1 | heavy_plus_rare | 2.80/2.96/3.04 | 2.83/2.97/3.13 | 2.82 → 2.83 |
| 1 | three_mixed_terms | 2.83/2.98/3.03 | 2.80/3.01/3.12 | 2.83 → 2.83 |
| 1 | planted_contiguous_phrase | 3.35/3.65/3.88 | 3.35/3.73/3.83 | 3.41 → 3.43 |
| 1 | natural_language_with_stop_words | 3.89/4.12/4.29 | 3.87/4.10/4.21 | 3.91 → 3.89 |
| 4 | broad_ubiquitous_single_term | 21.21/21.80/22.02 | 22.38/23.08/25.24 | 21.18 → 22.32 |
| 4 | broad_mid_single_term | 6.79/7.74/7.98 | 7.00/7.41/7.63 | 6.85 → 6.99 |
| 4 | selective_single_term | 1.24/1.35/1.42 | 1.39/1.69/2.80 | 1.25 → 1.44 |
| 4 | two_heavy_terms | 14.47/15.38/15.44 | 14.29/23.26/28.69 | 14.47 → 14.84 |
| 4 | heavy_plus_rare | 2.80/3.01/3.05 | 2.86/3.43/3.72 | 2.80 → 2.90 |
| 4 | three_mixed_terms | 3.63/4.91/7.89 | 3.53/3.81/4.24 | 3.79 → 3.57 |
| 4 | planted_contiguous_phrase | 4.92/5.90/8.54 | 4.76/4.98/5.01 | 5.02 → 4.78 |
| 4 | natural_language_with_stop_words | 4.50/4.76/5.28 | 4.43/5.44/5.96 | 4.51 → 4.51 |

Zipf process peak RSS MiB / DB bytes (old → new): 1 namespace(s): 44.32 → 44.11 / 41,869,312 → 41,869,312; 4 namespace(s): 31.25 → 30.58 / 41,472,000 → 41,472,000;

The production tradeoff is explicit: tied broad recall improves, but Zipf ubiquitous single-term p50/CPU worsens about 5–6%. The four-namespace memory p99 also worsens (25.13 → 56.08 ms). No general high-frequency speedup or improved tail guarantee is claimed. No write code changed, but measured write latency and process RSS are not uniformly lower.

Persistent `remember` p50/p95/p99 milliseconds (30 samples):

| Records / namespaces | Baseline | New |
| --- | --- | --- |
| 10,000 / 1 | 0.10/0.24/0.29 | 0.12/0.28/0.53 |
| 100,000 / 1 | 0.10/0.20/0.29 | 0.10/0.21/0.27 |
| 10,000 / 4 | 0.10/0.27/0.29 | 0.10/0.27/0.29 |
| 100,000 / 4 | 0.13/0.29/0.30 | 0.13/0.31/0.35 |

All four frozen suites, 83 queries total (25 + 25 + 26 + 7), retain every non-latency quality metric, per-query first-relevant rank and complete top-10 list. Namespace leaks and temporal-invalid results remain zero. These checks preserve existing lexical quality; they do not establish new semantic capabilities.

The separate auxiliary prototype uses 10k fixed-length ASCII documents: `status` plus 15 uniformly sampled terms from a 256-term vocabulary (seed 500), one namespace. Its `WITHOUT ROWID` primary key is `(term, tf, length, id DESC)`; each frequency group contributes at most k candidates. It measures 30 recalls/query and 100 individual writes/configuration. The prototype added 2,686,976 bytes to a 954,368-byte FTS database. Ubiquitous-query p50/p95/p99 fell from 9.50/9.74/9.79 to 0.97/1.05/1.11 ms, CPU 9.51 → 0.97 ms/query. However, individual-write p50/p95/p99 rose from 0.073/0.216/7.45 to 0.262/3.78/35.79 ms (100 samples). Building the auxiliary table took 326 ms. These measurements exclude a production recovery design and use a separate SQLite runtime. The prototype is rejected for this patch because of measured storage/write amplification and unimplemented lifecycle consistency. It remains evidence for a future explicit storage tradeoff, not a claimed production solution.

The same diagnostic's descending FTS scan reduced tied-query p50 from 9.50 to 7.78 ms despite increasing approximate outer VM steps from 190,100 to 220,100. This supports the top-k heap explanation: VM instruction count alone does not measure native sorting or FTS work.

**Integration decision: NO-GO against the stated v0.5 performance gates.** Native 100k one/four-namespace p95 and 10k one-namespace broad p95 still exceed the targets. The patch is a small exact optimization with documented regressions, not a completed v0.5 performance release.

## Persistent MCP and code path

Memory lifecycle, 100 cycles / 600 tool calls per binary: p50/p95/p99 = 0.430/1.410/1.624 → 0.441/1.385/1.521 ms. Child CPU = 0.456 → 0.455 seconds; RSS before close = 12.67 → 12.84 MiB; DB/WAL/SHM bytes = 4,353,672 for both. Each cycle asserts remember → recall → supersede → recall → forget → empty recall, including provenance and supersession IDs. The harness correctly rejects attempts to supersede a source record; lifecycle measurements use derived facts, matching the API contract.

Code fixture: 100 Rust files, 2,000 initial symbols; then 30 rounds of one-file modification, explicit refresh, grounded search and full incremental reconciliation. Every search returned the changed file and every reconciliation completed. No code retrieval implementation changed, so these are responsiveness checks, not demonstrated algorithmic gains.

| MCP operation | Baseline p50/p95/p99 ms | New p50/p95/p99 ms |
| --- | --- | --- |
| Explicit file refresh | 2.43/4.34/5.12 | 2.41/3.90/4.00 |
| Grounded code search | 2.20/3.55/4.57 | 2.24/2.66/2.69 |
| Incremental reconciliation | 4.79/8.97/13.08 | 4.82/5.35/5.45 |

Initial indexing was one sample only: 22.28 → 24.08 ms. Child CPU over the whole code workload was 0.384 → 0.380 seconds; RSS before close 15.41 → 15.66 MiB; DB/WAL/SHM bytes 421,888 for both. All observed grounded context responses were comfortably under one second on these fixtures; this does not establish a large-repository tail guarantee.

## Validation and limits

`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --locked`, and `cargo build --release --locked` pass. The suite includes exhaustive BM25 oracle agreement for one through four informative terms, migration preservation/rollback, killed-writer recovery, concurrent readers/writers, incremental refresh and lock release. Four MCP integration tests cover code indexing/grounding, candidate coverage, scoped memory supersession/forget and crash recovery. A new regression forces ties across the cutoff and repeated widening past expired and cross-namespace winners, including exhausted result sets.

This does not test power loss, actual AETHER Dots, Apple Silicon, million-record scale or long-duration resource behavior. Allocation counts were not instrumented. No claims of improved semantic precision, morphology, temporal ranking or context quality are made. The explicit memory lifecycle is also exercised for 100 cycles per binary through persistent MCP, checking evidence IDs, provenance, supersession links and absence after forget.

## Reproduce

Build `767dbba` from an archive in ignored storage with a separate target directory, preserve that binary, then build the working tree. Run binaries sequentially on the same host, without compilation during measurements:

```sh
BINARY bench-memory --sizes 10000,100000 --namespace-count 1
BINARY bench-memory --sizes 10000,100000 --namespace-count 4
BINARY bench-retrieval --size 100000 --namespace-count 1 --samples 50
BINARY bench-retrieval --size 100000 --namespace-count 4 --samples 50
target/release/midnight-harness memory-quality --binary BINARY --fixture benchmark/memory_quality_v1.json --split all --repeats 3
```

Repeat the evaluator for `memory_quality_v2`, `memory_quality_single_term_v1`, and `memory_retrieval_adversarial_v1`. Compare all non-latency quality metrics, per-query ranks and complete top-10 lists, not just aggregate recall. Do not publish raw outputs with host or path identifiers.
