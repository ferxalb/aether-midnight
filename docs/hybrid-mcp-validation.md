# Hybrid/MCP validation for v0.1.0-rc.6

This records a local same-host comparison of the compiled rc.5 release binary with the
development release binary. It is not a claim of perfect relevance, platform certification or
a published stable release. The [mode/ranking/privacy contract](hybrid-mcp.md) describes the
intentional behavior changes. Raw runs remain in ignored local storage; this report contains
only reviewed workload definitions, aggregate measurements and synthetic query identifiers.

## Functional checks

- `cargo test --locked`: 210 passed, zero failed, three ignored. The live-provider test remains
  ignored; automated cloud tests use the deterministic loopback mock.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --check`, `git diff --check`, and `cargo build --release --locked`: passed.
- Nine new MCP integration tests exercise real stdio/HTTP entry points, including consent
  revocation, project opt-out, HTTP remote grants, incremental embedding, transient failure
  without interactive retries, mode validation, memory scope/time restrictions, cache eviction
  and connection isolation. Unit regressions cover expiry/model/scope isolation, long-file
  phrase recovery, exact-symbol protection, literal boundaries, rank consistency across limits,
  and oversized queries rejected before provider loading.
  Two harness tests cover dependency-compatible fixture reversal and invalid dependency graphs.

The cache test checks request counts, not inferred latency: an identical query reuses its vector;
semantic/hybrid share it; lexical sends nothing; the 33rd distinct query evicts the oldest of 32.
Unchanged reindexing sends no embedding request. These are request-elimination guarantees for
the tested conditions, not an end-to-end speedup estimate.

## Existing OpenRouter index

Manual product-entry checks used a copy of an existing opt-in index, the same query and the same
stored vectors for both binaries. No source reindexing or source uploads were needed. Queries
were sent to the configured OpenRouter provider separately from the automated tests.

For `Privacy-first personal intelligence`, with limit 20 and token budget 8192:

| Mode | Before target-file rank | After target-file rank |
| --- | --- | --- |
| lexical | 2 | 1 |
| hybrid, three alternating before/after pairs | 12, 12, 11 | 1, 1, 1 |

All six hybrid CLI calls used hybrid without degradation. The varying old rank illustrates why
a single remote query cannot establish deterministic provider behavior. These samples establish
the specific literal-evidence correction, not general semantic relevance.

A real stdio MCP session also returned the target first in three consecutive default-mode
hybrid calls, with no degradation and identical final two result arrays. An earlier MCP session
hit the five-second query transport timeout on its first call: it returned explicit degraded
lexical results, still with the target first, and recovered hybrid on its next call. Remote
availability is not guaranteed; fallback is part of the contract. Timing data for these remote
calls is not used to claim improved latency.

## Frozen code quality

`midnight-harness retrieval-quality` ran the unchanged fixture and labels: 51 queries per
strategy, including 46 positive queries.

| Strategy | MRR before → after | Recall@5 before → after |
| --- | --- | --- |
| identifier decomposition | 0.978261 → 0.978261 | 0.978261 → 0.978261 |
| unicode61 comparison | 0.804348 → 0.804348 | 0.804348 → 0.804348 |

Every per-query reciprocal rank and recall metric is unchanged. The ordered top five changes
for `rust-complete_word`, `python-complete_word`, `typescript-complete_word` and
`tsx-complete_word` in both strategies (all four ask `retry policy`). Rust and TSX exchange
positions three and four, both relevant. This is an intentional consequence of the documented
phrase ranking contract, not an unchanged-order claim. No labels were edited.

The harness measures Midnight itself through fresh release CLI `search` processes, using
`wait4` for CPU/RSS, with indexing outside the query measurement. The following are sums over
51 query processes, with maximum peak RSS, not per-query latency estimates:

| Strategy | Wall s before → after | CPU s before → after | Peak RSS MiB before → after |
| --- | --- | --- | --- |
| identifier decomposition | 3.248 → 4.371 | 3.146 → 3.254 | 12.54 → 12.94 |
| unicode61 comparison | 3.242 → 3.545 | 3.143 → 3.411 | 12.76 → 12.76 |

Both aggregates got slower in this pair. The phrase lookup adds work; the elapsed differences
also include shared-host scheduling. This was one non-interleaved campaign pair, not a controlled
statistical latency experiment. There is no claim that all code queries got faster or that
these totals predict larger-repository performance.

## Frozen memory quality

All fixtures ran with `--split all --repeats 3`, with both forward and dependency-compatible
reverse insertion. Memory lexical ranking and memory RRF were not
changed. The entire per-query quality objects, including every rank, match before/after for
each comparison below:

| Fixture | Records | Queries | MRR before → after |
| --- | ---: | ---: | --- |
| memory_quality_v1 | 155 | 25 | 0.916667 → 0.916667 |
| memory_quality_v2 | 10027 | 25 | 0.891667 → 0.891667 |
| memory_quality_single_term_v1 | 10027 | 26 | 0.896825 → 0.896825 |
| memory_retrieval_adversarial_v1 | 7000 | 7 | 1.000000 → 1.000000 |

The before/after quality objects match in both insertion orders, including all metrics and
individual query ranks. The table's MRR values also hold for reverse insertion. A pre-existing
harness panic was fixed before the reverse comparison: a raw reversal inserted derived records
before their sources. Preparation now preserves the requested preference among dependency-ready
records while keeping sources and superseded records before their dependents. Missing references,
duplicate keys and cycles return errors before starting Midnight. The same corrected harness
drives both engine binaries; labels and record contents are unchanged.

This preserves existing quality, including its existing misses; it does not establish perfect
memory retrieval. The exhaustive-oracle and insertion-order tests in `src/retrieval_tests.rs`
also pass. The memory-quality harness's whole-process usage includes seeding, so it is not
presented as query-only CPU or throughput evidence here.

## Paired memory diagnostics

Both release binaries ran `bench-retrieval --size 10000 --samples 30` with one namespace. The
following are production `staged` measurements; fixture seeding and warmups precede samples.
Wall percentiles and thread CPU are measured inside Midnight, without an external client.

| Query class | Wall p95 ms before → after | CPU ms/query before → after |
| --- | --- | --- |
| broad ubiquitous single term | 5.741 → 5.753 | 5.008 → 4.881 |
| broad mid-frequency single term | 1.497 → 1.366 | 1.377 → 1.337 |
| selective single term | 0.294 → 0.240 | 0.247 → 0.236 |
| two heavy terms | 2.057 → 1.931 | 1.902 → 1.819 |
| heavy plus rare | 0.808 → 0.696 | 0.682 → 0.640 |
| three mixed terms | 1.483 → 1.070 | 1.113 → 1.032 |
| planted contiguous phrase | 4.372 → 2.681 | 2.700 → 2.500 |
| natural language with stop words | 3.762 → 2.871 | 3.152 → 2.779 |

Peak RSS for the diagnostic process was 24.03 → 22.84 MiB. Coverage labels and result counts are
unchanged. Ubiquitous-term p95 got slightly slower. Memory retrieval implementation is unchanged;
these differences do not establish a causal optimization.

`bench-memory --sizes 1000,10000 --namespace-count 4` produced the following internal operation
wall p95 values. Store/update/delete have 30 samples each; recall has 100. Setup is outside these
operation timers. Whole-campaign CPU includes setup, so it is not treated as query CPU.

| Records | Store ms before → after | Recall ms before → after | Supersede ms before → after | Delete ms before → after |
| --- | --- | --- | --- | --- |
| 1000 | 1.338 → 1.320 | 1.337 → 1.433 | 1.565 → 1.006 | 1.418 → 0.894 |
| 10000 | 1.222 → 1.291 | 3.473 → 3.630 | 1.223 → 1.133 | 1.614 → 0.958 |

Recall got slower at both sizes, and store got slower at 10000. Both quality objects are
identical. These are limited diagnostic pairs, not proof of a speedup or a scaling guarantee.

A separate `measure-mcp` pair excludes seeding from process measurement: 100 synthetic memories
seeded once through Midnight, the same database reused, 100 identical lexical recall calls,
one persistent MCP process per binary, one request in flight. `wait4` measures Midnight alone;
the handshake adds one request. Exact full ordered result arrays are identical across all calls.

| Process measurement | Before | After |
| --- | ---: | ---: |
| Wall s | 0.091681 | 0.091378 |
| CPU s (user + system) | 0.086190 | 0.085922 |
| Peak RSS MiB | 12.93 | 12.86 |

This tiny workload is a compatibility/resource smoke check, not a statistically supported
performance improvement. The 100k gate and a new platform/client release matrix are not covered
by these 1k/10k diagnostics.

## Reproduction and limits

Build with `cargo build --release --locked` and preserve the baseline binary before applying the
change. Use the same host, isolated provider-free configuration, fixture files, arguments and
insertion order for both binaries. Run each command separately without competing benchmarks:

```sh
midnight-harness retrieval-quality --binary BINARY
midnight-harness memory-quality --binary BINARY --fixture FIXTURE --split all --repeats 3
midnight-harness memory-quality --binary BINARY --fixture FIXTURE --split all --repeats 3 --insertion-order reverse
BINARY bench-retrieval --size 10000 --samples 30
BINARY bench-memory --sizes 1000,10000 --namespace-count 4
midnight-harness measure-mcp --binary BINARY --db SEEDED_DB --requests REQUESTS_JSON
```

Do not publish raw outputs: they can contain absolute paths, source snippets and environment
fingerprints. This validation does not run Claude Code/Codex graphical clients, certify unsigned
distribution, or close existing platform support gates. OpenRouter's protocol remains the
[documented embeddings API](https://openrouter.ai/docs/api/api-reference/embeddings/create-embeddings);
provider behavior outside the validated response/timeout contract remains external.
