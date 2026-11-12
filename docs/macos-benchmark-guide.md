# Native macOS benchmark guide

The local campaign uses a direct-child sampler rather than Activity Monitor's instantaneous CPU display. It records sampled RSS and the process high-water counter separately from macOS physical footprint, CPU time, page-ins, I/O, faults, threads, and context switches. Host snapshots record `vm_stat`, swap, `memory_pressure`, power, and thermal state. The Linux collector separately reports PSS, anonymous/file-backed memory, cgroup limits/events, pressure, and I/O when available.

## Reproduce the reference fixture

Run from the repository root on the Mac being measured. The smoke run creates a new timestamped artifact directory and a temporary 10k-file / 100 MiB source fixture. It does not modify a caller-owned repository or overwrite a committed result.

```sh
rustc --version
cargo --version
sysctl -n machdep.cpu.brand_string
sysctl -n hw.physicalcpu
sysctl -n hw.logicalcpu
sysctl -n hw.memsize
sysctl -n machdep.cpu.features
diskutil info -plist / | plutil -p -
pmset -g therm
memory_pressure -Q
cargo build --release --locked
target/release/aether-midnight bench --repo "$FIXTURE" --heldout-repo benchmark/retrieval_fixture --samples 5 --index-samples 5
```

The smoke mode uses one sample per measurement. For a standard run, use five samples and a longer bounded window:

```sh
target/release/aether-midnight bench --repo "$FIXTURE" --heldout-repo benchmark/retrieval_fixture --samples 5 --index-samples 5
```

For one controlled optimization comparison, keep the same binary configuration except for the intended source change, workload seed, selected worker count, output code version, power conditions, and cache condition. Save each output directory; never direct it to `benchmark/results.json`.

## Reference observations

Published benchmark reports omit machine models, host names, local paths, and other identifying environment details. Run on the actual target platform and keep unredacted measurements private. A result from one platform does not establish performance on another.

On this Mac, six workers formed the best measured throughput point for the 10k/100 MiB fixture; twelve workers did not improve throughput. Use these results as a device-specific observation, not as a Linux or Apple Silicon prediction. filesystem details withheld OS file cache was left intact. Creating a new database does not make the source fixture's OS pages cold.

For an independent one-off inspection, `/usr/bin/time -l target/release/aether-midnight ...` can provide resource-usage counters, `vmmap <pid>` can inspect VM regions, and Instruments can record allocator and CPU profiles. Keep diagnostic tools out of the timed process path and label their results separately. The built-in collector samples the direct child at 50 ms; sampled peaks can miss brief events, while `ru_maxrss` and `RUSAGE_INFO_V4` lifetime values are retained as separate OS high-water counters.

## Reproduction limits

The reference campaign contains five-sample full-index screens at 1/2/4/6/12 workers before the prepared-statement optimization and a five-sample six-worker after run. It also includes persistent MCP searches, idle CPU, one-file update visibility, and a five-query development retrieval suite. It does not establish Apple Silicon performance, Linux Kernel or VS Code status on the current revision, a 100k memory-record benchmark, filesystem-event watcher behavior, OS-cache-cold performance, thermal throttling over hours, or allocator-live-heap totals.

The v0.4 stabilization run now includes native Intel 1k/10k/100k memory measurements in one and four namespaces, an actual v0.3.1 binary comparison, and persistent MCP lifecycle measurements. See the changelog for results, interference limits and failing latency targets. The earlier reference-campaign limits above describe that earlier campaign.

## rc.7 validation

This procedure closes the open rc.7 measurement gates in the [checklist](release-checklist.md):
native paired numbers for the changes measured only on Linux, the local-model reuse that could not
be measured there, a large real repository, and real clients. Run it on the Mac being validated,
plugged in, with no other heavy work. Keep every output under one new directory outside the
repository (it can contain local paths); publish only aggregates, as `AGENTS.md` requires.

### 1. Build both binaries and the workloads

```sh
export RUN=$(mktemp -d /tmp/midnight-rc7.XXXXXX)
git fetch origin tag v0.1.0-rc.6
git worktree add "$RUN/rc6" v0.1.0-rc.6
(cd "$RUN/rc6" && cargo build --release --locked)
cargo build --release --locked
export OLD="$RUN/rc6/target/release/aether-midnight" NEW="$PWD/target/release/aether-midnight"
export H="$PWD/target/release/midnight-harness"
"$H" generate-fixture --output "$RUN/fixture"
"$H" rc7-workloads --repo "$RUN/fixture" --output "$RUN/wl"
```

`rc7-workloads` writes deterministic request files with the shapes and sizes of the Linux campaign
(`docs/performance.md`, 2026-11-11 and 2026-11-12): `code_lexical.json` (330 code requests),
`seed_memories.json` (10,000 memories), `knn_memory.json` and `knn_code.json` (60 distinct semantic
queries each), `memory_lexical.json` (300 lexical memory operations) and `remember_embed.json` /
`remember_no_embed.json` (100 stores each). The memory texts are generated by the harness, so they
are not byte-identical to the Linux run; compare old and new binaries on the same host only.

### 2. Seed once with the old binary, then copy

Every database is seeded by rc.6 and copied before measuring, so both binaries read the same data;
the copy for rc.7 is migrated by one `init` before any measurement.

```sh
export AETHER_MIDNIGHT_CONFIG_DIR="$RUN/cfg-lexical"      # no provider: lexical workloads
"$OLD" --db "$RUN/code.sqlite3" index "$RUN/fixture" --json > "$RUN/index-old.json"
"$H" measure-mcp --db "$RUN/code.sqlite3" --requests "$RUN/wl/seed_memories.json" --binary "$OLD" > /dev/null
for v in old new; do cp "$RUN/code.sqlite3" "$RUN/code-$v.sqlite3"; done
"$NEW" --db "$RUN/code-new.sqlite3" init --json | grep schema_version   # 8
```

### 3. Lexical code and memory workloads, interleaved

```sh
for round in 1 2 3 4 5; do
  for v in old new; do
    BIN=$OLD; [ $v = new ] && BIN=$NEW
    "$H" measure-mcp --db "$RUN/code-$v.sqlite3" --requests "$RUN/wl/code_lexical.json" \
      --binary "$BIN" --json-output "$RUN/code-$v-$round.json" > /dev/null
    cp "$RUN/code-$v.sqlite3" "$RUN/mem-$v.sqlite3"
    "$H" measure-mcp --db "$RUN/mem-$v.sqlite3" --requests "$RUN/wl/memory_lexical.json" \
      --binary "$BIN" --json-output "$RUN/mem-$v-$round.json" > /dev/null
  done
done
```

Compare `results` between the two binaries before reading any time: they must be identical except
`created_at` of stored memories and the index timestamps in `index_status`. Then report per request
class p50/p95 of `request_ms` and the `process` wall, CPU and peak RSS of every run.

### 4. Local-model reuse (the unmeasured rc.7 claim)

Needs the installed runtime and model (`midnight runtime install`, `midnight models pull
multilingual-e5-small`, about 2 GiB of free memory).

```sh
export AETHER_MIDNIGHT_CONFIG_DIR="$RUN/cfg-local"
"$NEW" setup --global --provider local --no-input
"$OLD" --db "$RUN/code.sqlite3" embed --namespace project:knn          # seed vectors with rc.6
"$OLD" --db "$RUN/code.sqlite3" embed --repo "$RUN/fixture"
for v in old new; do cp "$RUN/code.sqlite3" "$RUN/knn-$v.sqlite3"; done
"$NEW" --db "$RUN/knn-new.sqlite3" init --json > /dev/null
for round in 1 2 3; do
  for v in old new; do
    BIN=$OLD; [ $v = new ] && BIN=$NEW
    for w in memory code; do
      "$H" measure-mcp --db "$RUN/knn-$v.sqlite3" --requests "$RUN/wl/knn_$w.json" \
        --binary "$BIN" --json-output "$RUN/knn-$w-$v-$round.json" > /dev/null
    done
  done
done
```

rc.6 loads the model again for every distinct query, so its runs are expected to take minutes;
rc.7 should load it once per process. Report the first-request and remaining-request latencies
separately, the process CPU and peak RSS, and confirm identical `results`. Then measure
write-time embedding with the same configuration:

```sh
for k in embed no_embed; do
  cp "$RUN/knn-new.sqlite3" "$RUN/w.sqlite3"
  "$H" measure-mcp --db "$RUN/w.sqlite3" --requests "$RUN/wl/remember_$k.json" \
    --binary "$NEW" --json-output "$RUN/remember-$k.json" > /dev/null
done
```

### 5. Large real repository

```sh
git clone --depth 1 https://github.com/microsoft/vscode.git "$RUN/vscode"
export AETHER_MIDNIGHT_CONFIG_DIR="$RUN/cfg-lexical"
/usr/bin/time -l "$OLD" --db "$RUN/vs-old.sqlite3" index "$RUN/vscode" --json > "$RUN/vs-index-old.json"
/usr/bin/time -l "$NEW" --db "$RUN/vs-new.sqlite3" index "$RUN/vscode" --json > "$RUN/vs-index-new.json"
ls -l "$RUN"/vs-*.sqlite3
```

Record the commit SHA, index wall/CPU/peak RSS and database bytes for each binary, then run the
eight real-repository queries of `docs/performance.md` (`TODO`, `error`, `test`, `parse`, `config`,
`cache`, `main`, `timeout`) through `measure-mcp` against each database (requests of the same form
as `code_lexical.json`, with `repo_path` set to the clone) and compare results and latency.

### 6. Real clients

In a scratch Git repository (not a private one):

```sh
midnight connect claude-code --scope project --with-instructions
midnight connect codex --scope project --with-instructions
```

Start a session in each client, give it a small task, and record whether it calls `recall` at the
start, stores a `source` and then a `derived` memory citing it without being asked, and which
scope it used (`midnight memory scopes`). Repeat with an embedding provider configured and check
`embedding` in the `remember` responses and `recall` with `mode: "hybrid"`. Record client
versions and outcomes only; do not publish transcripts that contain private content.

Clean up with `git worktree remove "$RUN/rc6"` and delete `$RUN` once the aggregates are recorded.
