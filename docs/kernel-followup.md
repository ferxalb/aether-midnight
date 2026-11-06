# Kernel regression follow-up

This supersedes causal interpretations of the sequential samples in `kernel-paired-evidence.md`; it does not delete those observations. The retained historical baseline rebuilds byte-for-byte from `1f30289`. The retained historical candidate rebuilds byte-for-byte from local pre-publication commit `544d6f4`, not published `9653863`. `9653863` was rebased onto `5494559` and changed HTTP/WAL setup; its `src/memory.rs` is byte-identical to `544d6f4`. We measure both immutable candidate artifacts separately. All builds use the committed lockfile, identical release LTO/codegen settings and compiler. Original binaries, hashes, worktrees and raw output remain ignored. The historical commit is retained locally rather than assumed to be a published ref.

Production retrieval restores the original allocating classifier, Unicode normalization pipeline and eager hydration. Idempotency, immediate write transactions, maintenance, integrity verification and schema 5 remain intact. Borrowed token descriptors, ASCII normalization and deferred hydration are isolated behind the diagnostic-only `retrieval-experiments` feature, without a CLI/MCP production recall option. This conservative fallback avoids claiming that a tail regression was fixed by an unproved micro-optimization.

## Diagnostics

`paired_kernel BASELINE CANDIDATE SIZE NAMESPACES SAMPLES` is a Rust persistent-MCP harness. It constructs the deterministic Zipf fixture, fixes query time, clones the same closed/checkpointed schema-4 snapshot three ways, starts baseline twice and candidate once, warms connections, rotates order and namespace each sample, and asserts complete decoded response equality. Candidate migration occurs before timed recalls. Cross-process wall time includes pipes and JSON. The baseline duplicate measures identical-code variability. This is a full-path comparison of actual original binaries; it does not infer thread CPU from pipe timing.

`bench-retrieval --kernel-ab` (feature `retrieval-experiments`) holds persistence, connection, database, SQLite, corpus and SQL constant while rotating baseline, identical control, combined candidate and three independent ablations. It checks complete decoded results on every sample. Add `--kernel-fixed --kernel-broad` to reproduce the original fixed namespace and ubiquitous-query class directly in-process. Timing uses thread CPU and wall clocks. A separate work pass records fetch limits, SQL executions (number of limits), widening, inspected rows, term rejects, actual hydration counts/bytes, VM steps and sorts. SQLite cache deltas accompany acceptance samples. Cache misses do not establish physical reads. Work-pass timings are separate from acceptance measurements.

Compile `allocation-diagnostics` only for a separate diagnostic run. A forwarding System allocator counts Rust allocation/reallocation calls and requested bytes around individual recalls, excluding fixture setup and report serialization. It changes timing and must never supply acceptance latency. It does not count SQLite's C allocator, freed/live bytes, mmap pages or physical memory. This diagnostic-only unsafe allocator forwarding has no production build effect. RSS remains the independent process-level observation.

Kernel diagnostic reports admit at most 1,000 samples per path/class; the cross-process harness admits at most 2,000. Report vectors are diagnostic-only and included in shared process RSS; they are not attributed to an individual recall path. SQL stage counts exclude planner probes, and outer rows are not the number of documents scored internally by FTS5. That internal posting/scoring count is unavailable. Hydrated byte counts are logical string bytes, not allocator capacities.

All diagnostic corpora and result buffers remain bounded by existing fixture and retrieval limits; no corpus cache or larger retrieval budget is introduced. Early archived ablation output incorrectly labelled its namespace distribution as fixed; auditing the query loop identifies those runs as round-robin, as labelled in the aggregate report. Current output reports the actual distribution, and explicit fixed-namespace trials close that measurement-condition difference. No numeric samples were relabelled or discarded.

Both harnesses preserve WAL/NORMAL. Percentile definitions are unchanged, samples are retained in original order, and no outliers are discarded.

## HTTP locking

`9653863` added a 10 ms retry loop around journal-mode configuration after observing SQLITE_BUSY there. An operation location is evidence, not a confirmed connection-lifetime cause. This follow-up removes that polling loop. Connections first read the persistent journal mode and request WAL only when initialization is required. Busy handling otherwise remains SQLite's existing five-second policy; errors are returned without retrying whole writes or acknowledging incomplete operations.

The reproduced failure was `initialize WAL journal mode`, extended SQLite code 5, during the two-server HTTP test on a database that did not yet exist. Token creation had created only the token registry. Both servers had announced readiness before WAL/schema initialization, permitting concurrent first requests to initialize the empty database. HTTP now initializes and closes the bootstrap connection before announcing readiness; request connections remain independent. No idle keeper is retained: per-request close/checkpoint behavior and cache admission remain unchanged. This avoids changing opportunistic WAL checkpoint frequency while fixing startup ordering. Cross-process RSS results describe stdio MCP, not HTTP.

The test asserts that the database exists in WAL mode at schema 5 before the first request. This confirms a bootstrap/readiness race; it does not establish the low-level lock sequence of every historical busy error or prove immunity to unrelated contention. Ten repeated HTTP suites and full tests are recorded separately.

The explicit Rust `journal_mode_contention_diagnostic` compares concurrent open/configure/close loops with setters versus reads, recording extended SQLite codes and operation microseconds on failures. Maintenance/reopen stress uses separate connections for eight concurrent writers, keyed retries, forget and incremental maintenance; integrity is checked after joining them. Existing HTTP tests cover two server processes and multiple writers; migration rollback and killed-writer tests cover independent failure paths. A successful stress run does not establish a root cause or prove absence of future busy errors. SQLite connection ownership remains per request connection/session, with no connection shared concurrently and no global request serialization.

Native macOS is unavailable and remains unverified. Process termination tests do not simulate power loss; WAL/NORMAL still allows loss of recently acknowledged commits after OS crash or power failure. No stronger durability claim is made.

## Reproduce

Build original commits in separate target directories, retain the binaries, and check artifact hashes before timing:

```sh
git worktree add --detach target/kernel-followup/base 1f30289
git worktree add --detach target/kernel-followup/candidate 9653863
# Historical 544d6f4 is an additional locally retained pre-publication commit.
cargo build --release --locked --manifest-path target/kernel-followup/base/Cargo.toml --target-dir target/kernel-followup/build-base
cargo build --release --locked --manifest-path target/kernel-followup/candidate/Cargo.toml --target-dir target/kernel-followup/build-candidate
cargo build --release --locked
# Copy these release binaries before changing build features.
cargo build --release --locked --features retrieval-experiments
# Copy the diagnostic binary, then build the independent allocator diagnostic.
cargo build --release --locked --features allocation-diagnostics
```

After compilation stops, run `paired_kernel` with retained original binaries, then baseline versus restored production. Repeat 100k/four-namespace runs with 200 samples/path. Run uninstrumented `bench-retrieval --kernel-ab --size 100000 --namespace-count 4 --samples 200` twice; repeat at 10k/one and four namespaces and with `--tied-corpus`. Run the allocator build separately with 10 samples/path. The original `bench-memory` commands and all four frozen evaluator suites compare baseline against restored production. Raw per-call JSON, build/test logs and artifact hashes stay in ignored storage. [Aggregate evidence](kernel-followup-evidence.md) contains all classes, including slower ones.

Final fixed-namespace checks use the delivered production binary. Earlier round-robin comparisons and ablations retain their artifact identities; final HTTP bootstrap connection closing does not change retrieval code.

For the original fixed-namespace condition, append `--fixed-scope --broad-only` to the cross-commit command and use 300 samples/path in each of two trials. Compare `1f30289` against historical `544d6f4`, published `9653863`, and delivered production, retaining the duplicate baseline control. The matching in-process command is `bench-retrieval --kernel-ab --kernel-fixed --kernel-broad --size 100000 --namespace-count 4 --samples 200`, repeated twice without allocation instrumentation.
