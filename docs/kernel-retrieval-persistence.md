# Retrieval and persistence kernel

Historical record for `9653863`. The [follow-up](kernel-followup.md) supersedes retrieval optimization and HTTP setup behavior; idempotency, maintenance and integrity guarantees remain in production.


The implementation and paired measurements began from `1f30289`. The review reference `5494559181bbcac4383937bda0b4e40189728630` was initially absent from the local checkout. Before delivery to GitHub, fetching `main` made it available; this work was rebased onto it, preserving the newer README, history notes and HTTP accept-loop wakeup change. Retrieval and persistence code in those intervening commits is unchanged. Required checks were rerun after rebasing; the published benchmark measurements describe the earlier campaign, not a new HTTP timing comparison. The direct exact scan remains production default. No labels, relevance rules, embeddings, providers or vector storage change.

Candidate classification retains NFKD decomposition, combining-mark removal, lowercase expansion and alphanumeric token boundaries in precisely that order. Content tokens borrow slices from one normalized buffer. ASCII uses the standard-library lowercase routine because decomposition and mark removal are identities there; all other text retains the original Unicode pipeline. Coverage counts and contiguous phrase detection use a vector of borrowed token slices, including overlapping phrase starts. The initial repeated-iterator experiment is retained as diagnostic evidence; it was replaced after measured regressions. Ranked stages inspect borrowed SQLite text before constructing an owned record. SQL candidate enumeration and widening remain unchanged; this deliberately avoids introducing a cache whose validity would depend on snapshot changes between widening statements. The allocating reference remains in the regression test and historical benchmark path.

Retrieval retains the existing 64 KiB record input limit, 64 query-term limit, 500 reranking-candidate limit and bounded result windows. The new classifier has one record-sized normalized buffer (Unicode decomposition can expand it), a bounded vector of token descriptors (at most one 16-byte descriptor per normalized byte on 64-bit platforms), no per-token owned strings, no corpus cache and no retained corpus representation. These are working-set bounds, not an RSS ceiling; SQLite sorting, page cache and statement state remain separately measured. Batch admission remains 512 records/16 MiB. Source URI bytes, SQLite internal allocations and output serialization are not covered by the content limit.

## Operation keys

The additive library API is `remember_idempotent(path, key, &MemoryInput)`; existing `remember` and batch callers remain compatible. CLI `remember --operation-key KEY` and MCP `remember`'s optional `operation_key` use the same implementation. Batch operations do not accept operation keys. Keys contain 1–256 bytes without NUL and are scoped to a database and exact namespace, shared across clients. Do not use secrets as operation keys.

Schema 5 adds a disk-backed ledger keyed by `(scope, operation_key)` with a SHA-256 digest of the serialized input fields and the result's memory ID. Equality is byte-exact, including timestamp spellings; generated timestamps are excluded. The ledger stores no second content copy. An immediate transaction serializes lookup, provenance validation, supersession, row/FTS/counter writes and ledger insertion. Ordinary and batch writes also acquire their write reservation before provenance reads. SQLite's existing five-second busy timeout applies; persistence errors propagate without acknowledging success.

Same key/input returns the existing record without another insert or corpus-statistics change. Changed input gives an actionable conflict. Different keys and unkeyed calls preserve independent observations. Retries return the record's current temporal fields if it has since been superseded. A forgotten result produces an explicit error and cannot be resurrected. Ledger entries persist through maintenance so old keys cannot silently be reused. They grow on disk with keyed operations; no in-memory ledger cache is introduced.

## Maintenance, integrity and durability

`maintain --max-records N` (1–512, default 128) scrubs at most N forgotten rows and asks each FTS index to merge 16 pages in one transaction. FTS merge work is a SQLite work hint, not a strict wall-time or allocation bound. The scrub count is returned; a count equal to N means another batch may be needed. This operation does not VACUUM or truncate the WAL. `compact` remains the explicit full operation and reports a busy truncating checkpoint instead of silently proceeding as if compaction completed. Operations can finish earlier steps before a later full-compaction error; retry is safe.

`verify-integrity` checks SQLite integrity, foreign keys, both FTS structures, memory posting membership and namespace counters in one transaction. It reports inconsistency as an error. Contentless FTS structure checks do not prove every term equals freshly tokenized source text; exhaustive retrieval tests provide separate behavioral coverage. Existing provenance permits forgotten source rows to remain referenced, so verification does not require all referenced sources to be active.

Durability remains WAL with `synchronous=NORMAL`; no performance comparison relaxes it. SQLite transactions protect records, FTS, counters and ledger together against rollback and process termination. A process kill is not a power-loss simulation: acknowledged recent WAL commits can be lost after OS crash or power failure with NORMAL. Full compaction is not a substitute for backups or a stronger sync policy. Migration DDL, backfill and version changes share an immediate transaction. Tests inject migration failure and kill a writer with uncommitted memory, FTS/counter, ledger and DDL changes.

## Measurement

Raw output stays in ignored `target/kernel-evidence`. Use identical release binaries and sequential workloads on the same host, with compilation complete before timing:

```
BINARY bench-memory --sizes 10000 --namespace-count 1 --profile-recall
BINARY bench-retrieval --size 10000 --namespace-count 4 --samples 30
```

Repeat memory at four namespaces and retrieval at one namespace, and use `--scan-ab --tied-corpus` for tied distributions. The Zipf fixture covers selective, ubiquitous, multi-term and phrase queries. Frozen suites are evaluated with the existing evaluator and compared by quality metrics, ranks and full top-10 lists. Rust benchmark commands report wall percentiles, CPU and RSS; profile phases and SQLite counters are diagnostic evidence. In the staged path, `sqlite_candidate_step` includes Rust classification and hydration, so it must not be presented as isolated SQLite CPU. Allocation counts and precise live-byte peaks remain unavailable; removing token ownership establishes fewer allocation sites, not a measured allocator-wide count. macOS is unverified in this change.

Paired aggregate results and limitations are recorded in [the evidence report](kernel-paired-evidence.md).

Development validation observed intermittent `database is locked` failures in the existing concurrent HTTP write test. Immediate write reservations prevent provenance-read upgrades; added WAL/write-reservation error context makes future failures more actionable. Five subsequent full HTTP suite repetitions and a later locked full-suite run passed. These bounded repeats do not establish the cause of every earlier failure or eliminate busy errors under contention; callers must handle explicit persistence failures. No failed call is acknowledged as a successful write.

Post-rebase validation pinpointed a repeated HTTP write failure at `PRAGMA journal_mode=WAL` during connection setup. WAL setup now retries SQLITE_BUSY outside transactions within a five-second deadline; other errors still propagate immediately. This preserves WAL/NORMAL and does not retry transactions or acknowledged writes. Earlier benchmark numbers predate this setup-only fix.
