# Native macOS kernel validation

**Acceptance: FAIL.** The native memory latency targets fail. The additional
repository-archive structural-completeness check also fails. Synthetic code
indexing passes at 2/4/6/12 workers; its one-worker configuration fails the
3-second target. These decisions use native measurements, not Linux results.

Candidate: `f6bf25b9186dc3263cbd376c5be6b0504deadb7e`.
Reference: `1f302895232a856e71e657235fcaf6ce6654c5f7`, as identified by
`kernel-paired-evidence.md` and `kernel-followup-evidence.md`; neither the
historical optimization candidate nor v0.3.1 is substituted for this reference.
The checkout initially had a clean working tree on `main`. The requested
`kernel/retrieval-persistence` branch was absent locally and from the remote
heads query; the exact requested candidate commit was present. Detached,
ignored worktrees preserve both implementations. Native macOS execution was
confirmed; architecture, toolchain details, binary hashes and raw output remain
in ignored local storage. No external model API was used.

## Method and correctness

Both binaries use their committed lockfiles and identical default release
settings (`lto=thin`, one codegen unit, stripped). Compilation and correctness
jobs finished before acceptance timing. No latency samples are removed.
Percentiles use `sorted[ceil((n - 1) * p)]`.
The two committed dependency lockfiles are identical. Binary provenance comes
from the detached builds and retained hashes: existing Python wrappers label
the invoking checkout commit, which is not the baseline binary's source commit.
Their checkout labels are not used to identify the baseline in this report.

Formatting, strict locked all-target/all-feature Clippy, default locked tests,
retrieval-experiment locked tests, and locked release builds pass. Each test
configuration passes 52 unit and 13 integration tests; two internal diagnostics
remain ignored. This includes the exhaustive oracle, scope isolation, temporal
validity, provenance, forget/supersession, migration rollback, concurrent keyed
writes/conflicts, maintenance, MCP initialization, shutdown/restart and killed
writer recovery. Ten additional HTTP-suite executions pass (70 tests).
The added HTTP integration test covers scoped replay/conflicts, concurrent new
writes and reads with maintenance, and acknowledged-write survival/replay after
successful SIGINT shutdown and restart; it also passes ten focused repeats.
No lock failure reproduces, so there is no reproduced lock error code/timing
to attribute or suppress.
All four frozen memory suites, 83 queries total, preserve every non-latency
quality field, ordered top-10, first relevant rank, response bytes and context
estimate. Namespace leaks and temporal-invalid results are zero. Frozen-suite
timings overlap setup work and are excluded from performance conclusions.
The frozen code suites also preserve all decoded ordered results: 5 dev, 64
held-out and 51 identifier/literal queries, compared with an identical control.
[Their completeness flags](../benchmark/evidence/kernel-native-macos-code-frozen.json)
are explicit: the held-out comparison uses an incomplete structural index and
is not counted as a structural acceptance pass. Both binaries on the same clean
candidate archive report one `tree_sitter_parse_error` in `src/main.rs`.
A standalone parse confirms grammar `ERROR` nodes around conditional fields in
the `BenchRetrieval` match pattern (lines 486–498), independently of the 250 ms
parser budget. Mac-specific causality is not established; this run does not
change the shared parser or source syntax to suppress the failed gate.

The Rust harness in `scripts/native_kernel_pair.rs` creates deterministic
baseline-schema fixtures, checkpoints them, and copies each into three separate
databases. Candidate migration, startup and warmup precede timing. Each trial
compares baseline, identical baseline control, and candidate using persistent
stdio processes; all decoded response fields must match on every call. Two
trials cover 10k/100k records, one/four namespaces, Zipf s=1 (3,000 words,
12–40 tokens/record) and tied corpora. Zipf classes include ubiquitous, middle,
selective, two-heavy, heavy-plus-rare, three-term, planted phrase and natural
language queries. Tied corpora use the shared `namespace` term. Each class has
200 samples per path per trial, with cyclic execution order and namespace
rotation. CPU is child process CPU per query; sampled RSS and whole-process
`wait4` peak RSS are distinct. Rust allocation diagnostics are separate and do
not contribute acceptance timings.

## Reproduction

See the [reproducible commands](kernel-native-macos-commands.md) for isolated
locked builds, frozen suites, paired trials, standard benchmarks and diagnostics.
Raw destinations are ignored; timed jobs run sequentially after compilation.

## Paired acceptance measurements

Every one of 43,200 timed recalls preserves the full decoded response. The
[complete paired aggregates](../benchmark/evidence/kernel-native-macos-pairs.csv)
include all 72 class/layout/trial rows, including slower classes. Each latency
cell below is p50/p95/p99 ms for the ubiquitous Zipf term; CPU is mean ms/query
in baseline/control/candidate order.

| Records/namespaces, trial | Baseline | Identical control | Candidate | CPU B/control/C |
|---|---|---|---|---|
| 10k/1, 1 | 5.49/5.61/5.81 | 5.48/5.62/5.86 | 5.58/5.81/6.22 | 5.44/5.44/5.53 |
| 10k/1, 2 | 5.20/5.39/6.09 | 5.18/5.31/6.85 | 5.17/5.29/5.48 | 5.17/5.13/5.11 |
| 10k/4, 1 | 2.54/3.94/5.57 | 2.53/3.81/5.55 | 2.60/3.62/5.71 | 2.57/2.55/2.62 |
| 10k/4, 2 | 2.38/2.53/4.68 | 2.37/2.49/5.55 | 2.40/2.55/4.72 | 2.32/2.32/2.35 |
| 100k/1, 1 | 52.88/62.31/75.14 | 52.84/61.20/84.04 | 52.84/61.05/88.63 | 53.91/54.10/53.94 |
| 100k/1, 2 | 49.57/52.17/53.63 | 49.62/52.40/53.04 | 49.48/52.25/52.97 | 49.82/49.84/49.70 |
| 100k/4, 1 | 19.90/21.21/26.57 | 19.99/21.68/25.05 | 20.14/21.61/24.72 | 20.01/20.05/20.18 |
| 100k/4, 2 | 19.76/20.88/24.53 | 19.62/20.86/25.10 | 19.74/21.01/24.50 | 19.79/19.68/19.75 |

[Peak RSS aggregates](../benchmark/evidence/kernel-native-macos-rss.csv) cover
every layout and trial. At 100k/four namespaces, Zipf whole-process peak MiB is
16.82/17.01/16.89 in trial 1 and 16.88/16.42/17.02 in trial 2. At 100k/one
namespace it is 16.94/16.91/17.03 and 16.60/16.12/16.46. These are process
high-water marks, not allocator-live-heap measurements.

Candidate p99 exceeds both baseline copies in 28 of 72 rows. Five combinations
do so in both trials: 10k/1 planted phrase, heavy-plus-rare and natural language;
10k/4 heavy-plus-rare and natural language. For example, 10k/1 heavy-plus-rare
p99 B/control/C is 1.10/1.03/1.34 ms and 0.95/0.94/1.05 ms; CPU is
0.841/0.839/0.845 and 0.763/0.762/0.757 ms/query. A stable CPU penalty is not
demonstrated, but the tail differences are retained and their cause is not
established. Broad 100k/1 candidate p99 is slower in trial 1 and faster in
trial 2. Identical-code variability is evidence of variability, not proof of
the cause of any individual outlier or a blanket regression waiver.

Standard `bench-memory` runs retain 100 recall and 30 write samples/layout.
[All aggregates](../benchmark/evidence/kernel-native-macos-memory.csv) include
CPU for setup plus operations (not CPU/query), peak RSS and writes. Recall
p50/p95/p99 ms baseline → candidate:

| Records/namespaces | Baseline | Candidate | Whole-workload CPU seconds B → C |
|---|---|---|---|
| 10k/1 | 7.39/7.79/8.00 | 7.30/7.44/7.84 | 1.919 → 1.881 |
| 10k/4 | 3.33/3.47/3.85 | 3.30/3.47/3.63 | 1.243 → 1.245 |
| 100k/1 | 66.73/70.96/72.79 | 65.39/69.34/70.82 | 16.913 → 16.722 |
| 100k/4 | 24.22/25.16/25.81 | 23.36/24.25/24.76 | 8.704 → 8.561 |

Slower write tails are retained: 100k/1 write p95 1.084 → 1.104 ms and p99
1.111 → 1.119 ms; 100k/4 p95 1.064 → 1.081 ms and p99 1.079 → 1.264 ms.
10k/4 whole-workload CPU increases slightly. These runs are sequential pairs;
the cross-process experiment above supplies the rotated identical-code control.
Standard `bench-retrieval` also runs both binaries at every size/layout, with
100 samples/strategy/class and warm persistent connections. Its delivered
`staged` path's [complete aggregates](../benchmark/evidence/kernel-native-macos-retrieval.csv)
are retained separately from both historical and opt-in strategies. No
approximate strategy is used for acceptance.

## Separate work and allocation diagnostics

[Work counters](../benchmark/evidence/kernel-native-macos-work.csv) cover every
size/layout/distribution, ten diagnostic calls/class, using the delivered
reference path. For 100k/four-namespace ubiquitous Zipf recall, means are one
candidate SQL execution, zero widening passes, 18 outer rows inspected, 11
classifications/hydrations, 893.3 content bytes and 1,583 record bytes hydrated,
and 164,736 VM steps. Outer rows are not all internally scored FTS documents;
bytes are logical string lengths, not physical I/O. Adversarial oracle tests
exercise widening separately; zero widening on these fixtures does not imply
that widening is disabled.

[Allocation counters](../benchmark/evidence/kernel-native-macos-allocations.csv)
come from a separate allocator-enabled binary, 100k/four namespaces, ten
diagnostic calls/class. The production reference path averages 425.5 Rust
allocation/reallocation calls and 34,603.4 requested bytes per ubiquitous-term
recall. These exclude SQLite C allocations and are allocation traffic, not live
heap. These are candidate work/allocation diagnostics, not an actual-baseline
allocation comparison. The diagnostic command's experimental `candidate` label
selects rejected optimizations; only its `baseline` reference flags describe
the delivered production path in the published counter aggregates. No
instrumented timing contributes to acceptance.

## Native code indexing and refresh

The standard campaign uses the existing seed `20261103`, 10,000 Rust files /
100 MiB, five samples/configuration, 50 ms direct-child sampling, workers
1/2/4/6/12, and four update workers. Every full index is lexically and
structurally complete. OS file caches are not evicted. These are sequential
baseline/candidate campaigns, not an isolated attribution of small differences.
[All worker latency/CPU/RSS aggregates](../benchmark/evidence/kernel-native-macos-index.csv)
retain slower configurations.

| Workers | Full-index p95 ms B → C | 3,000 ms gate |
|---|---|---|
| 1 | 3806.17 → 4013.36 | Fail both |
| 2 | 2755.84 → 2737.68 | Pass both |
| 4 | 2254.04 → 2359.87 | Pass both |
| 6 | 2192.12 → 2209.90 | Pass both |
| 12 | 2294.35 → 2235.16 | Pass both |

Six-worker full-index p50/p95/p99 ms is 2164.65/2192.12/2192.12 →
2167.89/2209.90/2209.90; median process CPU seconds 4.588 → 4.613 and
p95 RSS high-water MiB 25.63 → 25.57. No worker or resource budget was enlarged
to turn a failure into a pass.

| Operation | n per binary | Baseline p50/p95/p99 ms | Candidate p50/p95/p99 ms | Gate |
|---|---|---|---|---|
| First grounded context, empty DB | 5 | 177.73/183.00/183.00 | 167.17/177.09/177.09 | Pass: p50 ≤1000, p95 ≤3000 |
| Warm persistent code search | 25 | 11.76/13.11/13.14 | 12.06/13.22/13.49 | Pass: p95 ≤50 |
| Campaign one-file visibility | 5 | 471.21/478.20/478.20 | 474.23/495.15/495.15 | Pass: p95 ≤1000 |
| Full reconciliation visibility | 30 | 284.78/294.93/295.63 | 300.49/330.28/343.52 | Pass: p95 ≤1000 |
| Direct-file refresh visibility | 30 | 10.36/11.32/11.43 | 10.65/12.08/12.50 | Pass: p95 ≤1000 |
| Useful partial result during indexing | 30 | 195.80/199.00/204.28 | 195.82/200.13/203.46 | Pass: ≤3000 |

All first-context samples are grounded, all warm queries hit their expected
files, all updates are fresh, and final reconciliation restores catalog
completeness. Both refresh paths are slower in the candidate;
[their complete aggregates](../benchmark/evidence/kernel-native-macos-refresh.csv)
are retained. The one-worker and four-worker index p95, six-worker CPU, and
twelve-worker median latency/CPU also increase; no causal performance claim is
made from these sequential pairs.
Every partial-result sample is useful and explicitly marked partial while the
indexer remains active, and every indexer exits successfully. Both final
SQLite integrity checks return `ok`.
[Partial-result aggregates](../benchmark/evidence/kernel-native-macos-partial.json)
retain the sample counts and correctness flags.

## Limits

The 10k one-namespace memory p95 target ≤5 ms fails in both paired Zipf trials
and standard memory measurement. The 100k targets ≤20 ms (one namespace) and
≤15 ms (four) fail in both paired trials and standard measurement. The
one-worker full-index p95 target ≤3,000 ms also fails. No blanket absence of
material regression is established: slower classes and sequential code-refresh
differences remain explicit, with their causes unresolved.

No reproducible macOS-specific kernel defect was established, so production
source, durability settings, retrieval defaults, relevance labels and resource
limits are unchanged. Changes are the HTTP integration coverage, Rust diagnostic
harness and sanitized evidence. Linux was not rerun; no shared runtime
correction is being represented as Linux-validated.

Process termination tests do not simulate power loss. Power-loss durability and
historical stall causality remain unverified. Linux evidence is retained as
Linux evidence; this campaign does not claim a cross-host latency bound.
