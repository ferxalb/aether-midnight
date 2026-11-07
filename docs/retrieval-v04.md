# v0.4 memory retrieval: class-staged exact top-k

This record covers the v0.4 work on high-fan-out memory recall: the measured root cause, the guarantees the engine now provides, the alternatives that were built and rejected, and the evidence. Raw benchmark output is kept in ignored local storage; only aggregate distributions and workload definitions appear here, per the benchmark privacy rule in `AGENTS.md`.

## State of the repository

The `codex/v031-reliable-memory-retrieval` branch named in the assignment was not present in this checkout or on the remote. Remote `main` (`bdc940f`) already contains the v0.3.1 BM25 fix (`0b83321`, "rank bounded memory candidates with BM25") plus the privacy commits, so `bdc940f` is the v0.3.1 baseline used for every comparison. No unpublished v0.3.1 work was assumed or reviewed. Work is on `perf/midnight-v04-retrieval-engine`; nothing was reset, overwritten, or force-pushed.

## Root cause of broad-query latency

Evidence (reproduced; Linux x86_64, 4 vCPU, 100k records, one namespace). The first row is the project binary (bundled SQLite 3.54.1); the isolation rows used the same corpus shape in a stock SQLite 3.53.1 harness, so treat their ratios as indicative:

| Measurement | Result |
| --- | --- |
| v0.3.1 recall of a term in every record, p50 / p95 | 76.9 / 98.5 ms (`bench-memory --namespace-count 1`) |
| Enumerating the same 93k matches with `count(*)` (no ranking) | about 3 ms |
| `ORDER BY rowid LIMIT 50` on the same matches | 0.02 ms |
| FTS5-only `ORDER BY rank LIMIT 50` (no join to `memories`) | about 100 ms |
| Same, with the join to `memories` and filters | about 111 ms |
| Same, scope term removed from `MATCH` | about 77 ms |

So the cost is `bm25()` evaluation per matching row (about 0.8 µs/row), not match enumeration, not the join (about 10%), and not Rust hydration. The scope term costs another quarter in a one-namespace database because FTS5 walks a posting list as long as the namespace.

Verified in the bundled SQLite source (`sqlite3.c`, 3.54.1):

- `fts5Bm25GetData` calls `xQueryPhrase` for **every phrase occurrence** in the expression to count hits for its IDF, and floors a non-positive IDF to `1e-6`. A term repeated in several sub-clauses is therefore counted several times per query.
- `FTS5_BI_ORDER_RANK` is `0x20`. `EXPLAIN QUERY PLAN` showing `INDEX 32:rM2` means FTS5 consumes a single-key `ORDER BY rank` itself, running an internal `SELECT rowid, rank … ORDER BY rank` over **all** matches (`fts5CursorFirstSorted`) before any row reaches the join. A two-key `ORDER BY` shows `INDEX 0` and is sorted by SQLite after the join.
- A SQL `LIMIT` therefore does not bound how many matches are ranked. The VM-step counter (about 1,315 steps in the earlier profile) counts bytecode steps in the outer statement; ranking happens inside the virtual table's `xNext`/`xColumn`, which the counter does not break out. We make no claim about physical posting-list page reads.

FTS5 exposes no per-term maximum, block-max, or impact-ordered data, so WAND, Block-Max WAND and MaxScore cannot run on top of it. They would need a second index.

## Guarantees

For queries with one to four informative terms the returned list is the exact top-k under this ordering, proven against an exhaustive oracle:

1. Class: contiguous exact phrase first, then more distinct informative terms.
2. Within a class, FTS5-compatible BM25 (k1 = 1.2, b = 0.75, column weights 0 / 1, global corpus statistics as FTS5 computes them).
3. Among exactly equal BM25 scores, the most recently remembered row is selected at the cut.
4. The selected rows are then presented in order of observation time, validity start, content, provenance, and row ID.

A row must contain at least two distinct informative terms (one, for a one-term query) to be eligible, as before. Namespace, deletion, and validity-time filters are exact SQL predicates on `memories`.

Not guaranteed:

- Queries with more than four informative terms use one bounded any-term BM25 window (five times the limit, at most 500) and are labeled `bm25_top_k`. Rows with one matching term can still consume part of that window.
- BM25 corpus statistics (IDF, average length) are global across namespaces because the FTS table is shared; another namespace's content can change scores, never membership.
- Lexical matching only: no stemming, no paraphrase (see limitations).
- `broad_terms: "recent"` is a deliberate non-exact opt-in (below).

`candidate_coverage` values: `complete`, `exact_top_k`, `bm25_top_k`, `recency_window`, plus the experimental `row_id_sample`. `complete` is reported only when every stage ran to exhaustion; filling the limit never claims it.

## Design

`recall_staged` in `src/memory.rs`:

- Because the class dominates BM25 in the final order, classes are enumerated best-first and enumeration stops once the limit is filled: phrase stage (`"phrase" AND t1 AND …`), all-terms AND, then "at least m of n" down to two. Stage expressions contain each term a uniform number of times, so within-class BM25 is a uniform multiple of BM25 over the query terms. If at most 128 rows have two or more terms (checked with an unranked probe), they are read once and classified exactly in Rust.
- Each stage is `SELECT rowid, rank FROM memory_fts … ORDER BY rank, rowid DESC LIMIT n` inside a subquery, `LEFT JOIN`ed to `memories` for filters. The two-key order is done by SQLite over FTS5 columns only; rows that lose never touch `memories`. A `LEFT JOIN` returns NULL columns for an ineligible winner so filtering is distinguishable from exhaustion, and the fetch widens only then.
- When the namespace holds at least half of all memories, the `scope_token` term is dropped from `MATCH` and `m.scope = ?` alone enforces the namespace (scores are identical because that column has weight 0).
- Schema 3→4 adds `memory_scope_counts`, maintained by triggers in the same transaction as the row change, so the planner sizes a namespace in O(log n). The migration is one `BEGIN IMMEDIATE` transaction with an in-place backfill from `memories`; `memories`, `memory_fts`, and all content are untouched. Tested from a real v2 database (through the existing 2→3 rebuild) and from a v3 database; counters are tested through single insert, batch rollback, soft delete, and hard delete.

## Decision record

| Approach | Latency | Index size / write cost | Exactness | Complexity | Verdict |
| --- | --- | --- | --- | --- | --- |
| A. v0.3.1: one FTS5 BM25 window, rerank in Rust | 95 ms (100k, one ns, broad term) | none | exact BM25 order, but the 50-row window is chosen by BM25 while the final order is class-first, so single-term documents and high-BM25 non-phrase documents crowd out targets (reproduced) | lowest | baseline |
| B. Class-staged exact FTS5 (chosen) | up to 19.5× faster at 100k (p50); phrase and natural-language queries at 1k are 0.5 to 0.8× (slower) | +1 page, +about 0.07 ms per `remember` | exact top-k for ≤4 terms, proven against the oracle | one planner function, one 4-line migration | **selected** |
| C. Sidecar impact-ordered postings with WAND / Block-Max WAND | not built; would make broad single terms sub-10 ms | estimate only: about 1.6M `(term, doc, tf)` rows for 100k records, roughly 35 to 40 MB against a 26 MB database, and an index update per remember | exact | a second search engine to migrate, repair, and keep consistent with FTS5 | rejected: cost not justified while B meets the measured need except for one class |
| D. v0.3 row-ID window sampling | fastest | none | Recall@1/5/10 = 0.286 on the positional suite | low | rejected in v0.3.1; kept only behind `retrieval-experiments` |

Option C's size is an estimate from posting counts, not a measurement.

### Rejected or reverted experiments (kept as evidence)

- **`rowid IN (SELECT … pair-AND)` eligibility restriction:** 268 ms against 28 ms; pathological. Pair-AND expressions as the match set were 2 to 4× faster and became the stage design.
- **Newest-first window for single terms present in half the namespace, as the default:** 10× faster, but the frozen dev case `common-status-single-token` lost both relevant records (dev Recall@10 0.923 → 0.885). BM25 length and frequency normalization still carries signal at the IDF floor, so my assumption that it ordered noise was wrong. It survives only as the explicit `broad_terms: "recent"` opt-in.
- **Multi-column post-join `ORDER BY` tie-break chain:** deterministic, but disables FTS5's native rank ordering and forces a `memories` lookup per match.
- **Native rank order plus a rerun with the chain when a tie straddled the cut:** ties at the cut are the norm for broad terms, so it ran twice (154 ms against 78 ms).
- **Streaming the native order and resolving the tie group in Rust:** 186 ms against 78 ms on the fully tied workload (every tied row hydrated or compared in Rust).
- **Per-class stages up to six terms:** an "at least m of n" expression repeats each term C(n−1, m−1) times, and `bm25()` walks every occurrence; 5 and 6 term queries rose from about 1 ms to 9 ms on a 128-record namespace, and one 9-term query reached 115 ms. Capped at four terms.
- **Non-uniform "pair-chain" expression for long queries:** weighted later terms more and changed a frozen dev rank; replaced by the uniform v0.3.1 expression.
- **Custom FTS5 auxiliary ranking function (skip doc-size lookups):** needs the unsafe FTS5 C API outside rusqlite's safe surface; not attempted.

## Correctness evidence

All of the following pass in `cargo test --locked` (42 tests):

- **Oracle validity:** the exhaustive BM25 oracle equals FTS5 `rank` to a relative error below 1e-9 on more than 500 rows.
- **Exact top-k agreement:** engine equals the oracle on 720 randomized queries (limit 3 and 10) over three multi-namespace corpora of 700 records each, with validity windows, future-dated rows, soft deletion, shuffled insertion, phrase slices, and stop-word padding.
- **Crowding defect (new finding):** with 800 short single-term documents, five long documents containing both terms are all lost by the v0.3.1 window and all returned by the new engine (asserted both ways).
- **Phrase class over BM25:** an exact-phrase record with lower BM25 than 800 shorter non-phrase records ranks first.
- **Temporal, supersession, forget, namespace:** current and historical queries at different instants, provenance preserved, forgotten rows excluded, zero cross-namespace rows, including a 90% / 10% skewed layout that omits the scope term.
- **Insertion order:** score profile identical across forward, reversed, and rotated insertion; content identical whenever no exact tie straddles the cut.
- **Independent labeled judgments** (`independently_labeled_long_term_memory_cases_meet_quality_gates`, printed with `--nocapture`): 9 cases, lexical subset Recall@5 = Recall@10 = 1.000, Recall@1 = 0.600, MRR = 0.800, nDCG@10 = 0.852, abstention 2/2, namespace leaks 0. The two morphology and paraphrase cases (`live` against `lives`, `allergies` against `allergic`) return nothing: 0/2 found, reported rather than gated. This suite is small and authored here; it is a regression check, not a generalization claim.
- **Frozen suites, v0.3.1 against v0.4, dev and held-out** (`memory_quality_v1`, `memory_quality_v2`, `memory_quality_single_term_v1`, `memory_retrieval_adversarial_v1`): every metric (Recall@1/5/10, Precision@5, MRR, nDCG@10, evidence completeness, temporal accuracy, stale-fact retrieval, false positives, namespace leaks) and every per-query first-relevant rank is identical. Labels were not changed. The first staged implementation did regress one dev case; that is how the recency default was caught and removed.
- **Migrations and durability:** v2 and v3 databases migrate with all memories intact; batch rollback leaves counters unchanged; interrupted-transaction recovery and concurrent read/write tests still pass; `wal_autocheckpoint` is unchanged (SQLite default of 1,000 pages).

## Performance evidence

Paired A/B inside one binary and one database (`bench-retrieval`), strategies interleaved per sample with rotating order, 100 samples each, warm persistent connection, default 2 MiB page cache, Zipf corpus (3,000-word vocabulary, 12 to 40 tokens/record), result limit 10. Platform: Linux x86_64, 4 vCPU, SQLite 3.54.1, Rust 1.97.0. This is **not** the Intel macOS acceptance gate and is not comparable to the numbers in the assignment.

Result lists can differ from v0.3.1 only where many rows tie exactly: v0.3.1's choice among equal BM25 scores is unspecified (in practice the oldest), v0.4 selects the newest. The frozen suites show no rank differences.

100k records, p50 / p95 / p99 in ms (v0.3.1 in parentheses):

| Query class | one namespace | four namespaces |
| --- | --- | --- |
| Term in 93% of records | 47.9 / 56.7 / 58.9 (95.2 / 119.1 / 132.0) | 18.9 / 22.5 / 26.4 (29.6 / 38.7 / 41.5) |
| Term in 24% of records | 11.3 / 15.4 / 16.5 (26.6 / 33.4 / 42.5) | 5.9 / 8.5 / 9.5 (8.5 / 12.2 / 13.9) |
| Selective single term (0.15%) | 0.25 / 0.36 / 0.86 (4.85 / 5.86 / 7.65) | 0.99 / 1.68 / 1.75 (1.75 / 3.05 / 5.17) |
| Two heavy terms | 14.7 / 17.9 / 20.0 (88.2 / 107.3 / 119.8) | 13.2 / 13.9 / 14.6 (36.2 / 37.2 / 37.5) |
| Heavy plus rare term | 2.4 / 3.4 / 3.9 (28.3 / 36.2 / 40.7) | 2.2 / 4.1 / 4.2 (10.0 / 15.3 / 17.0) |
| Three mixed terms | 2.3 / 3.0 / 3.7 (13.3 / 17.5 / 20.7) | 2.9 / 4.9 / 5.1 (5.7 / 9.3 / 9.5) |
| Planted contiguous phrase | 2.8 / 4.0 / 14.9 (12.7 / 17.0 / 29.8) | 3.8 / 5.6 / 6.5 (5.0 / 5.6 / 7.7) |
| Natural language with stop words | 3.1 / 3.8 / 5.2 (12.5 / 16.1 / 19.3) | 3.5 / 3.6 / 4.1 (4.7 / 4.9 / 5.6) |

At 10k records every class is faster at p50 (1.9 to 6.8× in one namespace; 1.3 to 2.4× in four). At 1k records the single-term, two-term, and three-term classes are 1.15 to 3.8× faster, but **phrase and natural-language queries are slower in absolute terms**: 0.87 against 0.73 ms and 1.12 against 0.68 ms in one namespace, 1.07 against 0.67 ms and 1.33 against 0.63 ms in four (p95 up to 1.9 ms). Several short stages each carry fixed statement cost that one window did not.

Standard `bench-memory` workload (a term in every record; every record ties), recall p50 / p95 / p99 ms, v0.3.1 against v0.4:

| Records | One namespace | Four namespaces |
| --- | --- | --- |
| 1k | 1.46 / 2.06 / 4.82 → 1.00 / 1.09 / 1.69 | 1.02 / 1.28 / 1.54 → 0.87 / 1.12 / 1.42 |
| 10k | 7.86 / 10.02 / 12.49 → 5.98 / 6.72 / 8.35 | 3.38 / 5.10 / 6.93 → 2.78 / 3.06 / 4.22 |
| 100k | 76.5 / 93.3 / 109.0 → 59.7 / 69.8 / 81.0 | 24.8 / 31.6 / 32.7 → 21.7 / 27.1 / 32.3 |

CPU time per query tracks wall time (single-threaded; the paired tables report both). Peak RSS did not increase (12.2 MiB against 14.4 MiB at 100k, one namespace). Idle CPU is unaffected: nothing runs in the background.

Persistent MCP, including JSON-RPC serialization, 20 repeats per query over the frozen suite: 128-record background p50 / p95 / p99 = 1.20 / 1.94 / 2.07 ms (v0.3.1: 1.05 / 1.92 / 2.31); 10,000-record background 8.10 / 10.21 / 12.83 ms (v0.3.1: 9.69 / 12.70 / 12.81).

Opt-in `broad_terms: "recent"` for a term in 93% of records: 4.1 ms at 100k in one namespace and 2.5 ms in four, about 12× and 8× faster than exact ranking there; it is labeled `recency_window` and is not exact.

### Write path and storage

`remember` p50 / p95 at 100k: 0.67 / 0.84 ms (v0.3.1) against 0.74 / 1.08 ms (v0.4) in one namespace and 0.67 / 1.02 against 0.74 / 0.89 in four; the counter triggers add about 0.07 ms p50 (about 10%) to a single insert. Batches amortize it. Database size grows by one 4 KiB page (25,796,608 → 25,800,704 bytes at 100k); the FTS index is unchanged. The WAL checkpoint threshold was not touched.

## Targets

| Target (Intel Mac reference, p95) | Result on this Linux host (v0.3.1 in parentheses) |
| --- | --- |
| 1k recall ≤ 2 ms | met for every class: 0.14 to 1.91 ms (v0.3.1: 0.27 to 1.99) |
| 10k recall ≤ 5 ms | met for every class except the 93% term in one namespace, 5.7 ms (11.4); all others ≤ 3.3 ms |
| 100k, four namespaces ≤ 15 ms | met for every class except the 93% term, 22.5 ms (38.7); two heavy terms 13.9 (37.2) |
| 100k, one namespace ≤ 20 ms | **not met** for the 93% term, 56.7 ms (119.1); met for every other class, including two heavy terms 17.9 (107.3) and the 24% term 15.4 (33.4) |
| Selective one-term ≤ 3 ms | met: 0.36 ms in one namespace, 1.7 ms in four (5.9 and 3.1) |

The 93%-of-records exact case is the remaining bottleneck: ranking tens of thousands of matches costs about 0.5 µs per match inside FTS5 and cannot be pruned without a second index (option C) or an approximate mode.

## Limitations

- Exact BM25 over a broad term is still O(matches). Only `recent` (non-exact) or a sidecar index would make it constant-time.
- More than four informative terms are bounded and not class-exact.
- Small corpora (about 1k records) pay roughly 0.4 to 0.7 ms more on phrase and natural-language queries.
- Lexical only; morphology and paraphrase recall are not addressed.
- Global BM25 statistics across namespaces.
- Native Intel macOS measurements were recorded in the v0.4 stabilization run (since removed); broad exact queries still fail the documented native latency targets. Apple Silicon remains untested. The Linux results above remain platform-specific.
- The intermittent explicit-refresh advisory-lock failure was reproduced with an inherited descriptor and fixed with an explicit unlock guard. The original test passed 50 repeated runs.
- The 1M-record workload was not run (the benchmark caps at 100k by design).

## Reproduce

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --release --locked

# paired A/B (staged vs v0.3.1 vs opt-in recent), per size and layout
./target/release/aether-midnight bench-retrieval --size 100000 --namespace-count 1 --samples 100 > /tmp/ab.json
# the JSON above already contains the paired staged/v0.3.1 numbers

# standard memory scaling workload
./target/release/aether-midnight bench-memory --sizes 1000,10000,100000 --namespace-count 1

# frozen quality suites through a persistent MCP process
target/release/midnight-harness memory-quality --fixture benchmark/memory_quality_v1.json --split heldout
```

For a v0.3.1 binary, build `bdc940f` in a separate worktree and run the same commands. Do not commit raw output: it contains host and path identifiers.

The macOS gate uses the same commands on the native Intel Mac; see `docs/macos-benchmark-guide.md`.
