# v0.2 Part 2 validation

The release baseline is rc.7 at `4427450`. The Part 1 tip `dff35cb` is available for attribution. Phase A adds schema 10 memory provenance,
policy-checked MCP recall layers, duplicate suggestions and budget packing. Phase B reuses
Tree-sitter boundaries for new code embeddings and adds cloud query timeout and retry
controls. Phase C adds experimental native distribution and npm package assembly.

## Release gates

| Gate | Status |
| --- | --- |
| Historical Intel macOS locked release build | PASS before the final rc.8 tree |
| Locked development check on rc.8 follow-up | PASS |
| Strict Clippy and tests | PASS in CI on Linux and macOS for `1084416`; Windows test suite fails (known) |
| Paired frozen retrieval, token, watcher, daemon and migration measurements | PASS on one Intel macOS host, 2026-11-13 ([performance](performance.md)); `bench-retrieval`, `bench-memory`, semantic and hybrid modes and local-model runs NOT MEASURED |
| Additional ONNX Runtime archive and library digests | PASS |
| Linux and Windows native builds and smoke tests | NOT RUN |
| npm publish, signing and notarization | NOT RUN |

## Open risks

Memory needs full cross-layer and held-out multilingual evidence. Duplicate suggestions
retain both observations. CLI layering, automatic review dates, a durable embedding queue,
and a separate trust-discounting score remain open. Windows repository-relative reads have
a handle-based implementation. Private Windows ACLs, named-pipe daemon support and
signing workflow integration remain open. Model selection and int8 require held-out
quality, measured memory use and runtime parity.

Before publishing rc.8, close the macOS retrieval and migration gates,
then run the local model bake-off. Follow with native Linux and Windows verification, and
finish with signed artifact and npm provenance checks.

## Pending validation commands

Run on an approved validation host, one workload at a time. Keep raw output in ignored
storage. Use a separately built or checksum-verified rc.7 release binary. The isolated
config directory prevents saved cloud configuration from affecting lexical comparisons.

```sh
out="$PWD/target/rc8-validation"
mkdir -p "$out/config" "$out/rc7-source"
export AETHER_MIDNIGHT_CONFIG_DIR="$out/config"
git archive 4427450 | tar -xf - -C "$out/rc7-source"
CARGO_TARGET_DIR="$out/rc7-target" nice -n 19 cargo build --release --locked -j2 \
  --manifest-path "$out/rc7-source/Cargo.toml" --bin aether-midnight
export MIDNIGHT_RC7_BINARY="$out/rc7-target/release/aether-midnight"
nice -n 19 cargo build --release --locked -j2
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --release --locked --no-fail-fast -- --test-threads=2

h="$PWD/target/release/midnight-harness"
candidate="$PWD/target/release/aether-midnight"
for side in rc7 rc8; do
  bin="$candidate"
  if [ "$side" = rc7 ]; then bin="$MIDNIGHT_RC7_BINARY"; fi
  for limit in 5 10; do
    "$h" retrieval-quality --binary "$bin" --limit "$limit" > "$out/$side-code-$limit.json"
    "$h" token-report --binary "$bin" --limit "$limit" > "$out/$side-token-$limit.json"
  done
  for suite in memory_quality_v1 memory_quality_v2 memory_quality_single_term_v1 memory_retrieval_adversarial_v1; do
    for order in forward reverse; do
      "$h" memory-quality --binary "$bin" --fixture "benchmark/$suite.json" --split all \
        --insertion-order "$order" > "$out/$side-$suite-$order.json"
    done
  done
  for split in dev heldout; do
    "$h" memory-quality --binary "$bin" --fixture benchmark/memory_quality_v3.json \
      --split "$split" > "$out/$side-memory-v3-$split.json"
    "$h" semantic-code-quality --binary "$bin" --split "$split" --local \
      > "$out/$side-semantic-$split.json"
  done
  "$bin" bench-retrieval --size 100000 --namespace-count 1 --samples 100 > "$out/$side-bench-retrieval.json"
  "$bin" bench-memory --sizes 1000,10000,100000 --namespace-count 1 > "$out/$side-bench-memory.json"
done
for limit in 5 10; do
  "$h" token-compare --before "$out/rc7-token-$limit.json" --after "$out/rc8-token-$limit.json"
done
"$h" watch-latency --binary "$candidate" --samples 30
"$h" watch-idle --binary "$candidate" --seconds 60
"$h" daemon-measure --binary "$candidate" --clients 8 --writes 50 --requests 200
```

Compare complete frozen memory `retrieval_quality` objects and frozen code per-query
paths, ranks and metrics. Report paired latency, CPU and peak RSS, including slower
workloads. The v3 runner currently exercises explicit namespaces. Cross-layer and
multilingual memory quality require additional isolated cases and verified local vectors.
The semantic harness measures the first search process, which includes model initialization
and query work. A candidate model and int8 bake-off still require verified artifacts.

On native runners, run the commands in `.github/workflows/ci.yml` and the non-publishing
jobs in `.github/workflows/macos-release.yml`, including `npm pack`. Test the isolated
installer and update procedure in the release checklist. Signing, real client sessions
and npm provenance are separate release gates.
