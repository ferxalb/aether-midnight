# v0.5 correction: restore the original exact scan default

This corrects `36eb46c`. The original direct FTS scan is again the production default. Descending traversal remains available only to the paired benchmark. No schema, writes, durability settings, relevance rules, coverage labels or API inputs change. There is no new auxiliary index or production score probe.

## Reproduction and cause

Before changing the default, a single release binary interleaved original, descending and production paths on one database/connection, rotating order and queried namespace. Every sample asserted identical ordered result IDs. At 100k/four namespaces:

| Corpus / samples per path | Original p50/p95/p99 ms | Descending p50/p95/p99 ms |
| --- | --- | --- |
| Tied / 300 | 23.57/24.08/24.76 | 20.44/21.02/21.33 |
| Zipf ubiquitous / 200 | 20.35/20.85/21.22 | 21.96/22.63/22.91 |

The Zipf regression reproduces. EXPLAIN shows that descending adds a nested coroutine and changes the FTS traversal from index flags 0 to 192. The extra scan/sorter feeding work pays off when equal scores continually replace the top-k heap, but not with varied BM25 scores. Namespace size, term count and high match frequency do not distinguish these workloads: all have one token and the same namespace layout.

The historical 56.08 ms tied p99 did **not** reproduce as a consistent plan regression. The old measurement used 100 sequential samples; the unchanged nearest-index percentile rule makes its p99 equal its maximum. Its two largest samples were 32.75 and 56.08 ms. That record has no per-call CPU/cache trace, so its exact cause cannot now be established.

A subsequent instrumented 400-sample run observed stalls in every path: descending maximum wall/CPU = 107.58/36.15 ms; original = 84.13/29.97 ms; corrected production = 75.01/27.68 ms. Substantial off-CPU delay is confirmed, but scheduling versus physical I/O is not distinguished. SQLite cache misses at those calls were 101/121/422 respectively; these counters do not measure physical disk reads. No evidence establishes an exclusive descending-plan p99 failure. We neither discard outliers nor change the percentile definition to claim a fix.

## Planner decision and regression coverage

Production deterministically chooses the original exact statement. A 16-row rank probe is retained only in diagnostics: at 100k/four namespaces it cost p50 3.68 ms on tied data and 4.19 ms on Zipf, enough to erase the tied four-namespace saving and add substantial Zipf overhead. BM25 initializes global phrase statistics even when only 16 scores are requested. A focused regression also constructs 16 tied leading scores followed by hundreds of varied-score records, showing that this probe is not a reliable global tie signal. Namespace-count or query-length heuristics cannot resolve the same-layout counterexample. No adaptive cache or writer-maintained statistics were introduced.

Tests force both exact statements through tied cutoff/widening cases, varied term frequency/document lengths, cross-namespace and expired rows, then insertion and deletion. Both match the exhaustive BM25 oracle. A plan regression asserts that production uses the original statement, not the descending one. Existing one-through-four-term oracle tests remain unchanged. The tradeoff is deliberate: restoring the safest default gives back the tied-workload speedup; this is not a universal speedup.

## Measurement method

Native Intel macOS, same host; warm persistent connections, default SQLite cache, no compilation during measurements. These are shared-host observations, not exclusive-host tail guarantees. Raw output remains ignored. `bench-retrieval --scan-ab` rotates all three strategies and namespaces; captures ordered per-call wall/CPU/cache counters; checks equality on every sample; and emits plans and the rejected probe's cost. The production path and forced-original path are separate same-SQL controls. CPU/RSS sampling and the score probe occur outside timed recall calls. SQLite cache miss differences also depend on which traversal ran immediately before a call.

The paired matrix uses 200 samples/path except 400 for 100k/four-namespace tied and 250 for its Zipf case. Standard `bench-memory`, the original eight-class `bench-retrieval`, all frozen suites and persistent MCP are also compared between the actual previous and corrected binaries. Their separately run distributions are not substituted for the interleaved diagnosis.

## Interleaved correction matrix

For ubiquitous single terms, p50/p95/p99 milliseconds. “Before path” forces descending, exactly as `36eb46c` did for these queries. “Corrected” is production; the final column forces the original SQL as a same-run control. Process RSS is shared by all three strategies and cannot be attributed to an individual path.

| Records / namespaces / corpus | Before path | Corrected | Original control p99 | Mean CPU ms before → corrected | Shared peak RSS MiB |
|---|---|---|---|---|---|
| 10,000 / 1 / tied | 5.48/5.79/7.00 | 6.93/7.38/9.81 | 7.72 | 5.48 → 6.95 | 16.32 |
| 10,000 / 1 / zipf | 5.71/6.02/6.41 | 5.34/5.64/6.18 | 5.95 | 5.72 → 5.35 | 17.11 |
| 10,000 / 4 / tied | 2.35/2.49/2.63 | 2.61/2.75/2.92 | 3.00 | 2.36 → 2.61 | 15.84 |
| 10,000 / 4 / zipf | 2.44/2.60/2.79 | 2.26/2.38/2.50 | 2.64 | 2.45 → 2.27 | 16.30 |
| 100,000 / 1 / tied | 50.95/52.80/54.18 | 66.39/69.01/70.64 | 71.32 | 51.06 → 66.62 | 18.65 |
| 100,000 / 1 / zipf | 55.99/62.63/69.97 | 53.01/59.90/73.16 | 75.86 | 56.14 → 53.18 | 18.95 |
| 100,000 / 4 / tied | 19.34/21.65/25.09 | 22.81/25.19/28.48 | 32.99 | 19.67 → 23.02 | 18.30 |
| 100,000 / 4 / zipf | 20.56/21.50/22.12 | 19.41/20.30/21.19 | 20.95 | 20.60 → 19.43 | 18.54 |

No universal p99 improvement is established. The four-namespace tied maximum still reached 75.01 ms in corrected production; other paths also stalled. The one-namespace Zipf p99 increased versus descending in this matrix, although it was below the same-SQL original control. The 10k/one-namespace tied production p99 also exceeded its identical-SQL control. These are reported, not hidden behind better medians. A return to identical original SQL removes the introduced algorithmic risk; it does not certify a host-independent tail bound.

The other single-term Zipf classes (same interleaved runs), p50/p95/p99 ms:

| Records / namespaces / class | Descending | Corrected | CPU ms before → corrected |
|---|---|---|---|
| 10,000 / 1 / broad_mid_single_term | 1.56/1.64/1.80 | 1.46/1.53/1.58 | 1.56 → 1.46 |
| 10,000 / 1 / selective_single_term | 0.24/0.28/0.39 | 0.23/0.25/0.33 | 0.25 → 0.23 |
| 10,000 / 4 / broad_mid_single_term | 1.00/1.08/1.16 | 0.91/0.99/1.10 | 1.01 → 0.92 |
| 10,000 / 4 / selective_single_term | 0.41/0.48/0.58 | 0.36/0.44/0.57 | 0.38 → 0.33 |
| 100,000 / 1 / broad_mid_single_term | 13.26/13.73/14.17 | 12.59/12.99/13.96 | 13.28 → 12.60 |
| 100,000 / 1 / selective_single_term | 0.37/0.44/0.48 | 0.35/0.41/0.49 | 0.37 → 0.35 |
| 100,000 / 4 / broad_mid_single_term | 6.52/6.85/7.05 | 5.97/6.34/6.77 | 6.52 → 6.00 |
| 100,000 / 4 / selective_single_term | 1.30/1.37/1.79 | 1.14/1.23/1.46 | 1.31 → 1.15 |

## Standard benchmark comparison

Actual `36eb46c` and corrected binaries, sequential same-host runs. `bench-memory`: 100 persistent recall samples, 30 persistent writes per size/layout. CPU covers the whole workload, including setup and writes; peak RSS is cumulative across sizes. Database sizes are after measured operations. Values are p50/p95/p99 milliseconds.

| Records / namespaces | Recall before → corrected | Remember before → corrected | Whole-workload CPU seconds | Peak RSS MiB | DB bytes both |
|---|---|---|---|---|---|
| 10,000 / 1 | 5.49/5.73/6.25 → 6.51/6.85/7.50 | 0.10/0.24/0.29 → 0.10/0.23/0.27 | 1.72 → 1.89 | 18.04 → 16.64 | 2,711,552 |
| 100,000 / 1 | 51.69/54.98/58.97 → 65.15/67.51/67.96 | 0.10/0.20/0.26 → 0.10/0.19/0.27 | 14.70 → 16.97 | 29.36 → 28.54 | 26,001,408 |
| 10,000 / 4 | 2.45/2.78/2.96 → 2.47/2.57/2.85 | 0.11/0.28/0.35 → 0.09/0.25/0.27 | 1.37 → 1.22 | 19.50 → 16.67 | 2,695,168 |
| 100,000 / 4 | 20.70/21.69/22.71 → 22.45/23.61/24.05 | 0.13/0.30/0.31 → 0.12/0.27/0.29 | 8.58 → 8.56 | 31.12 → 29.24 | 25,563,136 |

The original eight-class `bench-retrieval`, 100k records, 40 samples/strategy/class. All classes are listed, including slower ones. Multi-term SQL did not change; differences there are observations rather than evidence of a new algorithmic effect.

| Namespaces / class | Before p50/p95/p99 ms | Corrected p50/p95/p99 ms | CPU ms before → corrected |
|---|---|---|---|
| 1 / broad_ubiquitous_single_term | 55.08/56.98/57.63 | 50.55/51.95/52.73 | 54.72 → 50.59 |
| 1 / broad_mid_single_term | 12.76/13.80/13.89 | 11.93/12.65/13.05 | 12.77 → 11.99 |
| 1 / selective_single_term | 0.38/0.56/0.58 | 0.34/0.38/0.49 | 0.40 → 0.35 |
| 1 / two_heavy_terms | 14.77/16.13/16.32 | 14.46/15.66/17.39 | 14.90 → 14.63 |
| 1 / heavy_plus_rare | 2.79/2.97/3.02 | 2.66/2.82/2.84 | 2.80 → 2.66 |
| 1 / three_mixed_terms | 2.68/2.85/2.92 | 2.65/3.08/3.26 | 2.69 → 2.69 |
| 1 / planted_contiguous_phrase | 3.42/3.60/3.70 | 3.19/3.38/3.68 | 3.41 → 3.19 |
| 1 / natural_language_with_stop_words | 3.92/4.74/5.52 | 3.55/3.79/3.86 | 3.94 → 3.58 |
| 4 / broad_ubiquitous_single_term | 21.83/22.63/40.56 | 18.94/19.43/19.72 | 21.86 → 18.99 |
| 4 / broad_mid_single_term | 6.93/7.72/11.44 | 5.89/6.49/6.50 | 7.02 → 5.98 |
| 4 / selective_single_term | 1.36/1.52/1.54 | 1.13/1.26/1.30 | 1.38 → 1.14 |
| 4 / two_heavy_terms | 14.17/15.15/15.95 | 13.13/13.89/14.59 | 14.19 → 13.20 |
| 4 / heavy_plus_rare | 2.74/3.06/3.12 | 2.56/2.73/3.24 | 2.76 → 2.59 |
| 4 / three_mixed_terms | 3.44/3.80/50.25 | 3.31/3.42/3.77 | 3.48 → 3.29 |
| 4 / planted_contiguous_phrase | 4.70/4.93/41.43 | 4.33/4.49/4.49 | 4.73 → 4.35 |
| 4 / natural_language_with_stop_words | 4.19/4.38/4.61 | 4.04/4.29/4.52 | 4.18 → 4.05 |

Eight-class benchmark peak RSS MiB / DB bytes before → corrected: 1 namespace(s): 44.57 → 42.86 / 41,869,312 → 41,869,312; 4 namespace(s): 31.53 → 28.78 / 41,472,000 → 41,472,000;

All **83 frozen queries** retain every non-latency quality metric, per-query rank and complete top-10 list. Namespace leaks and temporal-invalid results remain zero. Labels were not changed.

## Persistent MCP validation

Two persistent MCP processes, one per binary, share each seeded database. Recall order alternates per sample; namespace rotates; all decoded results are asserted identical. Each corpus/layout uses 150 paired samples. Seeding uses a separate process, excluded from child CPU/RSS measurements. Child CPU includes startup and three warm calls; RSS is sampled before shutdown. Values are p50/p95/p99 ms.

| Records / namespaces / corpus | Before | Corrected | Child CPU seconds | RSS MiB | DB bytes |
|---|---|---|---|---|---|
| 10,000 / 1 / tied | 5.22/5.43/6.03 | 6.79/6.98/7.46 | 0.86 → 1.10 | 12.57 → 12.48 | 2,424,832 |
| 10,000 / 1 / zipf | 5.49/5.67/6.03 | 5.14/5.44/5.63 | 0.90 → 0.84 | 12.59 → 12.60 | 4,235,264 |
| 10,000 / 4 / tied | 2.34/2.47/2.52 | 2.58/2.72/4.08 | 0.42 → 0.46 | 12.78 → 12.77 | 2,433,024 |
| 10,000 / 4 / zipf | 2.42/2.51/2.84 | 2.25/2.37/2.55 | 0.43 → 0.40 | 12.80 → 12.79 | 4,235,264 |
| 100,000 / 1 / tied | 51.24/52.93/54.17 | 65.72/67.45/68.75 | 7.92 → 10.12 | 14.84 → 14.83 | 23,764,992 |
| 100,000 / 1 / zipf | 55.22/57.90/58.72 | 50.20/52.66/54.09 | 8.53 → 7.78 | 14.58 → 14.64 | 39,759,872 |
| 100,000 / 4 / tied | 19.48/20.58/20.98 | 22.79/24.19/24.68 | 3.05 → 3.56 | 14.96 → 14.77 | 23,711,744 |
| 100,000 / 4 / zipf | 20.70/21.56/22.22 | 19.59/20.30/20.92 | 3.24 → 3.06 | 14.58 → 14.74 | 39,682,048 |

Memory lifecycle: 100 cycles/600 calls per binary; evidence, supersession and absence after forget asserted. Before → corrected p50/p95/p99 = 0.41/1.35/1.58 → 0.42/1.35/1.65 ms; child CPU 0.443 → 0.446 seconds; RSS 12.62 → 12.41 MiB; DB/WAL/SHM bytes 4,353,672 → 4,353,672.

Code-refresh stability: 100 files/2,000 initial symbols, 30 refresh → grounded search → reconciliation cycles per binary. Every search found the changed file and every reconciliation completed.

| Operation | Before p50/p95/p99 ms | Corrected p50/p95/p99 ms |
|---|---|---|
| refresh_ms | 2.36/4.36/5.24 | 2.34/2.93/5.53 |
| search_ms | 2.20/2.50/2.94 | 2.18/2.44/2.88 |
| incremental_reconcile_ms | 4.72/5.76/6.45 | 4.73/5.89/6.11 |

Cold indexing (one sample, anecdotal): 22.17 → 21.52 ms. Code-workload child CPU 0.374 → 0.375 seconds; RSS 15.48 → 15.36 MiB; DB/WAL/SHM bytes 421,888 → 421,888.

## Gates and decision

Formatting, strict all-target/all-feature clippy, locked tests and locked release build pass. Tests include migration rollback, killed-writer recovery, concurrent readers/writers, indexing locks and incremental refresh. All original performance/relevance evidence is preserved; no auxiliary-index experiment was repeated or integrated.

**NO-GO against the v0.5 performance and strict tail acceptance gates.** The repeated Zipf algorithmic regression is removed by the safest default, but there is no universal p99 improvement, no causal trace for the old 56.08 ms sample, and the original broad exact latency still exceeds targets. These bounded checks do not establish actual Dots integration, power-loss behavior, exclusive-host tail stability or long-duration resource behavior. No semantic-quality improvement is claimed.

## Reproduce

```sh
cargo build --release --locked
BINARY bench-retrieval --scan-ab --tied-corpus --size 100000 --namespace-count 4 --samples 400
BINARY bench-retrieval --scan-ab --size 100000 --namespace-count 4 --samples 250
```

Repeat at 10k/100k and one/four namespaces. `staged_direct` is the original scan, `staged_descending` the rejected default, and `staged` production. Without `--scan-ab`, the existing v0.3.1/production/opt-in-recency benchmark remains available. `--tied-corpus` requires `--scan-ab`. No forced strategy is added to the recall CLI or MCP contract.
