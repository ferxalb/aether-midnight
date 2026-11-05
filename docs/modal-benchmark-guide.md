# Modal CPU benchmark guide

`scripts/modal_bench.py` is the remote Linux CPU laboratory entry point. It uses the current Modal Python SDK Sandbox API, writes an immutable local run artifact, and never creates a GPU task or always-on service. Benchmark source trees, SQLite databases, WAL/SHM files, and temporary scratch data live on each sandbox's local ephemeral filesystem; no Modal Volume is mounted on the SQLite path. Network access is blocked for the measured sandbox. The image build installs Rust and compiles the release binary before measurements begin.

## Setup

Use Python 3.11 or newer and the current supported Modal SDK. The recorded campaign ran with Modal Python SDK 1.6.1; the runner's Sandbox API and local CLI entry points were also checked against SDK 1.6.3, the latest release when this guide was updated. Authenticate with a named profile before starting a run; never put credentials in the command line or manifest.

```sh
python3 -m venv /tmp/aether-midnight-modal-venv
/tmp/aether-midnight-modal-venv/bin/pip install --upgrade modal
/tmp/aether-midnight-modal-venv/bin/modal profile current
```

Estimate resources and cost before connecting to Modal:

```sh
/tmp/aether-midnight-modal-venv/bin/python scripts/modal_bench.py --mode smoke --dry-run
```

## Resource matrix and execution modes

The initial screening matrix requests equal CPU request and hard limit and equal memory request and hard limit:

| Tier | Physical CPU | Memory ceiling | Worker counts |
| --- | ---: | ---: | --- |
| Constrained | 0.5 | 256 MiB | 1, 2 |
| Minimal | 1 | 512 MiB | 1, 2 |
| Balanced | 2 | 1 GiB | 1, 2, 4 |
| Performance | 4 | 2 GiB | 2, 4, 6 |
| High parallelism | 8 | 4 GiB | 4, 8 |

`smoke` uses one sample per worker configuration, `standard` uses five, and `stress` uses thirty. The initial campaign screens all tiers but is not a Cartesian sweep across every worker count. Override tiers, workers, repetitions, timeout, budget, or output directory explicitly when needed.

```sh
# Bounded one-sample screen, estimated maximum under the default $10 cap.
/tmp/aether-midnight-modal-venv/bin/python scripts/modal_bench.py --mode smoke --budget-usd 10

# Routine comparison on selected tiers, with three samples per worker count.
/tmp/aether-midnight-modal-venv/bin/python scripts/modal_bench.py --mode standard \
  --tier balanced --tier performance --samples 3 --budget-usd 10

# Local cost estimate only; no Modal request is made.
/tmp/aether-midnight-modal-venv/bin/python scripts/modal_bench.py --mode stress \
  --tier balanced --samples 30 --dry-run
```

The planner estimates sandbox CPU and memory cost using current Modal Sandbox list prices, reserves a configurable amount for image build and control-plane overhead, and refuses runs whose worst-case reservation exceeds `--budget-usd`. For the initial five-tier smoke screen at a 600-second sandbox timeout, the estimate was $0.397621 sandbox compute plus a $5.00 image/control reserve, or $5.397621 total. This is a conservative estimate, not a billing API result. Check current official pricing before authorizing a larger run.

The planner also sums reservations recorded by earlier result artifacts under the same output root. During this investigation, four launch attempts accumulated a conservative $20.8055 in per-run reservations; the default dry-run now rejects another screen at a cumulative estimate of $26.2031. This intentionally blocks further Modal jobs under this output root. It is not an invoice: actual billing is unavailable to this runner, and the measured sandbox compute portion was only a small fraction of the reservation. The failed attempts and incomplete constrained-tier result remain preserved.

Each run creates a unique directory under `benchmark/runs/modal/` containing `result.json`, the planned resource configuration, SDK version, commands, raw worker results, and failures. It does not overwrite a baseline. Remote runs that fail before measurement are retained as failed artifacts.

## Recorded screening result

The November 5, 2026 UTC screen used one sample per worker configuration on a Linux x86_64 gVisor Sandbox. The Modal run used SDK 1.6.1; the successful four tiers have no source-tree fingerprint because that launcher version did not pass one into the container. A later constrained-only retry records source commit `aedadd122ac42f04250900bc64dc607a60e53903`, a dirty worktree, and uploaded-tree SHA-256 `f9fda658a7ac2d30208a9d2118b882fc2b2bc6e1c9b3b941d022a2e056729bc5`. These are screening points, not reliable p95 estimates.

| Tier | Workers | Index wall (ms) | CPU seconds | Peak RSS (MB) | Catalog / structure |
| --- | ---: | ---: | ---: | ---: | --- |
| 0.5 CPU / 256 MiB | 1 | 12,309 | 6.55 | 51.7 | complete / incomplete |
| 0.5 CPU / 256 MiB | 2 | 11,981 | 5.70 | 60.2 | complete / incomplete |
| 1 CPU / 512 MiB | 1 | 4,541 | 3.64 | 57.7 | complete / complete |
| 1 CPU / 512 MiB | 2 | 4,244 | 3.64 | 59.0 | complete / complete |
| 2 CPU / 1 GiB | 1 | 4,497 | 4.46 | 57.9 | complete / complete |
| 2 CPU / 1 GiB | 2 | 3,265 | 3.59 | 59.8 | complete / complete |
| 2 CPU / 1 GiB | 4 | 3,434 | 4.31 | 70.3 | complete / complete |
| 4 CPU / 2 GiB | 2 | 1,624 | 2.76 | 57.5 | complete / complete |
| 4 CPU / 2 GiB | 4 | 1,420 | 2.93 | 58.7 | complete / complete |
| 4 CPU / 2 GiB | 6 | 1,423 | 3.02 | 64.8 | complete / complete |
| 8 CPU / 4 GiB | 4 | 2,600 | 5.31 | 58.3 | complete / complete |
| 8 CPU / 4 GiB | 8 | 2,650 | 5.36 | 76.9 | complete / complete |

Each row is n=1, so p50/p95/p99 all equal the one observed point and should not be treated as stable percentiles. In the best observed configuration (4 CPU request/limit, 4 workers), throughput was about 7,041 files/s and 70.4 MiB/s, or about 30.0 CPU-seconds per GiB of eligible source. Its database plus SQLite sidecars was 18,722,816 bytes. The sandbox exposed Linux RSS but not cgroup memory counters or a CPU model. The constrained retry reported two and five Tree-sitter parse-error files for one and two workers; no OOM was reported, and the catalog stayed complete. That older artifact contains counts only; the current runner records up to 100 affected paths for future incomplete results.

Across five development queries, the completed tiers returned Recall@1 0.9, Recall@5/10 1.0, and MRR 1.0. Query counts are too small for a held-out quality claim. The 4 CPU/4 worker sample had 7.04 ms warm retrieval, 52.4 ms process-inclusive MCP round trip, and 213 ms one-file visibility; these are single-sample observations.

The first all-tier screen did not return a measurement for the constrained tier. A subsequent image build failed because the release build ran from the wrong directory; after that was fixed, a constrained retry failed because Git was absent from the image. Those attempts remain as failed artifacts. The next constrained run reached the index operation but produced structurally incomplete results. Do not treat any of these as complete successful samples.

## Data and interpretation

The current campaign measures the deterministic 10,000-file / 100 MiB fixture and records worker-count screening, process/cgroup memory where available, CPU seconds, throughput, database and WAL size, index completeness, warm retrieval correctness, update visibility, startup, and MCP latency. Incomplete structural indexes include a bounded list of per-file parser failures when SQLite can be read. `memory.current`, `memory.peak`, cgroup memory events, CPU throttling, pressure, I/O, PSS, and private memory are reported when the sandbox exposes them. Process RSS and cgroup usage are different scopes.

The SQLite database and WAL must remain on local scratch storage during timing. These are database-cold runs with an OS cache that may be warm; the runner does not claim a cold OS page cache. A resource-limit failure or incomplete structural index is a failed observation and remains in the artifact. Resource reservation is not a guarantee that a sandbox receives that much physical CPU at every instant; retain CPU model, architecture, scheduling, throttling, and cgroup evidence when Modal exposes it. The recorded Sandbox exposed an unknown CPU model and no cgroup counters, so it cannot establish a hard memory ceiling from observed cgroup usage.

The broader 50k/100k corpus, pinned Linux Kernel and VS Code checkouts, 100k-memory workload, concurrent indexing/search, cancellation recovery, and idle soak are not part of the initial smoke workload and must be added as separately labeled workloads before claiming coverage.
