# Native macOS kernel validation commands

Companion to [the validation report](kernel-native-macos-validation.md).

## Reproduction

Run from the repository root; all destinations below are ignored. Build first,
then run measurements sequentially without other compilation or benchmark jobs.

```sh
git worktree add --detach benchmark/runs/native-kernel/baseline 1f302895232a856e71e657235fcaf6ce6654c5f7
git worktree add --detach benchmark/runs/native-kernel/candidate f6bf25b9186dc3263cbd376c5be6b0504deadb7e
# In candidate worktree:
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo test --locked --features retrieval-experiments
# In each worktree, retaining separate default binaries before feature builds:
cargo build --release --locked
# Copy scripts/native_kernel_pair.rs into baseline/examples/native_pair.rs:
cargo build --release --locked --example native_pair
# From root, for N=1 and then N=2:
benchmark/runs/native-kernel/baseline/target/release/examples/native_pair \
  benchmark/runs/native-kernel/bin/baseline \
  benchmark/runs/native-kernel/bin/candidate benchmark/runs/native-kernel/trial-N
# For both binaries and each of the four frozen memory fixture JSON files:
target/release/midnight-harness memory-quality --binary BINARY --fixture FIXTURE --split all --repeats 1 --json-output PRIVATE_OUTPUT
# For each default binary, namespace count 1 and 4:
BINARY bench-memory --sizes 10000,100000 --namespace-count N
# For each default binary, size 10000/100000 and namespace count 1/4:
BINARY bench-retrieval --size SIZE --namespace-count N --samples 100
# For each default binary, separately and sequentially:
target/release/aether-midnight bench --repo "$FIXTURE" --heldout-repo benchmark/retrieval_fixture --samples 5 --index-samples 5
target/release/aether-midnight refresh-file --repo "$FIXTURE" --path src/00000.rs  # measure under /usr/bin/time -l
  --samples 30 --output PRIVATE_OUTPUT
target/release/midnight-harness generate-fixture --output benchmark/runs/native-kernel/code-fixture
BINARY bench --repo benchmark/runs/native-kernel/code-fixture \
  --heldout-repo benchmark/retrieval_fixture --samples 100 --index-samples 5 \
  --stress-cycles 10
# That bench invocation supplies partial-index timings, not archive acceptance.
# Separate archive correctness comparison (preserves completeness failures):
mkdir -p benchmark/runs/native-kernel/heldout-archive
git archive f6bf25b | tar -x -C benchmark/runs/native-kernel/heldout-archive --exclude benchmark
benchmark/runs/native-kernel/baseline/target/release/examples/native_pair \
  benchmark/runs/native-kernel/bin/baseline benchmark/runs/native-kernel/bin/candidate \
  benchmark/runs/native-kernel/code-frozen --code benchmark/runs/native-kernel/heldout-archive
```

Redirect benchmark JSON and stderr to unique ignored paths. Preserve default
binaries before building the candidate with `--features retrieval-experiments`
and separately `--features allocation-diagnostics`. With the diagnostic binary,
run `bench-retrieval --kernel-ab --size SIZE --namespace-count N --samples 10`
for every layout, once with Zipf and once adding `--tied-corpus`. With the
allocator binary, run the same command at 100k/four namespaces without
`--tied-corpus`. These diagnostic timings are excluded from acceptance.
