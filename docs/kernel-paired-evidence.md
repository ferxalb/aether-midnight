# Paired Linux kernel evidence

Baseline `1f30289`, delivered candidate working tree, identical release settings, same host, sequential runs without compilation. Warm persistent connections; shared-host measurements. No latency outliers removed. WAL/NORMAL unchanged. Raw output stays ignored. All 83 frozen queries retain every quality metric, first-relevant rank and full top-10 list; zero namespace leaks or temporal-invalid results. One evaluation repeat per suite.

## Zipf retrieval

30 samples per strategy/class/layout. Production staged path below. Latency is p50/p95/p99 ms; CPU is mean ms/query.

10k records, 1 namespaces. Peak RSS MiB 9.96 → 9.43; database bytes 4444160 → 4448256.

| Class | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| broad_ubiquitous_single_term | 4.790/12.144/27.334 | 4.338/4.606/6.584 | 6.154 → 4.441 |
| broad_mid_single_term | 1.318/1.720/1.872 | 1.166/1.260/1.319 | 1.381 → 1.182 |
| selective_single_term | 0.162/0.410/0.450 | 0.111/0.175/0.179 | 0.205 → 0.118 |
| two_heavy_terms | 1.725/2.244/2.385 | 1.506/1.798/1.798 | 1.803 → 1.555 |
| heavy_plus_rare | 0.514/0.820/0.984 | 0.424/0.546/0.635 | 0.549 → 0.438 |
| three_mixed_terms | 0.812/1.271/1.353 | 0.641/0.764/0.789 | 0.825 → 0.651 |
| planted_contiguous_phrase | 2.010/3.026/4.041 | 1.914/3.384/3.569 | 2.142 → 2.050 |
| natural_language_with_stop_words | 2.257/2.642/6.382 | 2.050/2.521/2.535 | 2.396 → 2.074 |

10k records, 4 namespaces. Peak RSS MiB 10.21 → 9.27; database bytes 4415488 → 4419584.

| Class | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| broad_ubiquitous_single_term | 1.986/2.444/2.772 | 1.996/2.290/2.425 | 2.038 → 2.015 |
| broad_mid_single_term | 0.758/0.952/0.972 | 0.743/0.912/0.936 | 0.775 → 0.772 |
| selective_single_term | 0.247/0.851/3.522 | 0.228/0.386/0.398 | 0.392 → 0.250 |
| two_heavy_terms | 1.591/2.094/2.176 | 1.580/2.173/2.550 | 1.638 → 1.656 |
| heavy_plus_rare | 0.618/0.901/1.852 | 0.622/1.002/1.202 | 0.689 → 0.680 |
| three_mixed_terms | 0.887/1.193/1.235 | 0.848/0.995/1.272 | 0.898 → 0.866 |
| planted_contiguous_phrase | 2.226/5.911/9.384 | 2.273/5.280/6.560 | 2.829 → 2.502 |
| natural_language_with_stop_words | 2.546/2.997/3.128 | 3.272/3.610/4.701 | 2.542 → 3.120 |

100k records, 4 namespaces. Peak RSS MiB 10.85 → 9.75; database bytes 41472000 → 41476096.

| Class | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| broad_ubiquitous_single_term | 18.601/20.106/22.469 | 18.115/20.922/39.405 | 18.696 → 18.617 |
| broad_mid_single_term | 5.676/6.913/9.034 | 5.866/6.930/8.645 | 5.848 → 5.940 |
| selective_single_term | 0.970/1.440/1.893 | 0.999/1.259/1.532 | 1.040 → 1.023 |
| two_heavy_terms | 12.918/14.802/15.218 | 12.071/17.237/31.313 | 13.004 → 13.047 |
| heavy_plus_rare | 2.622/3.661/3.827 | 2.176/2.633/11.313 | 2.722 → 2.513 |
| three_mixed_terms | 3.084/5.997/11.986 | 2.632/2.870/2.994 | 3.499 → 2.646 |
| planted_contiguous_phrase | 4.274/5.085/6.273 | 3.678/4.425/18.911 | 4.310 → 4.106 |
| natural_language_with_stop_words | 3.858/4.953/6.451 | 3.278/3.680/4.603 | 4.057 → 3.334 |

## Tied retrieval

10k records; 30 samples/path, interleaved direct/descending/production controls; production below.

| Namespaces | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| 1 | 6.893/9.683/11.370 | 5.916/6.424/6.855 | 7.204 → 5.996 |
| 4 | 2.351/2.649/2.875 | 2.518/3.131/5.827 | 2.385 → 2.617 |

## Memory workload

100 recall and 30 write samples/layout. CPU covers setup, writes and profiling, not CPU/query. RSS is whole-process peak. 10k runs enable diagnostic recall profiling; 100k runs do not.

| Records/namespaces | Recall baseline → candidate | Write baseline → candidate | CPU seconds | Peak RSS MiB |
|---|---|---|---|---|
| 10k/1 | 7.419/10.346/11.663 → 6.764/12.709/14.608 | 0.544/1.035/1.307 → 0.563/0.856/0.950 | 9.614 → 9.058 | 14.11 → 13.27 |
| 10k/4 | 2.736/3.179/5.902 → 2.620/3.268/3.548 | 0.508/0.751/1.315 → 0.502/0.725/0.973 | 4.131 → 3.691 | 14.04 → 13.00 |
| 100k/4 | 22.984/24.706/44.746 → 23.022/27.702/50.995 | 0.494/0.635/1.033 → 0.558/0.909/0.912 | 7.120 → 6.939 | 12.08 → 10.93 |

## Cost attribution

Diagnostic ranked-window path, 10k/one namespace, 100 samples. These phase timings do not isolate production staged classification from SQLite. Serialization CPU is unavailable; its wall time is shown.

| Phase | Baseline p50 wall/CPU ms | Candidate p50 wall/CPU ms |
|---|---|---|
| sqlite_candidate_step | 7.9662/7.9610 | 7.4849/7.4865 |
| candidate_record_hydration | 0.0307/0.0309 | 0.0299/0.0301 |
| candidate_text_tokenization | 0.0423/0.0423 | 0.0165/0.0167 |
| lexical_scoring | 0.0085/0.0087 | 0.0084/0.0087 |
| response_serialization | 0.0062/unavailable | 0.0062/unavailable |

| Counter | Baseline p50 | Candidate p50 |
|---|---|---|
| candidates_returned | 50 | 50 |
| candidates_hydrated | 50 | 50 |
| sqlite_vm_steps | 1264 | 1264 |
| content_bytes_hydrated | 2245 | 2245 |
| record_bytes_hydrated | 5421 | 5421 |

These results include slower classes and tails and do not establish a universal CPU or latency gain. Production candidate classification removes owned token strings; historical strategies retain allocating token vectors but share the equivalent ASCII normalization fast path. Allocator-wide counts, isolated staged SQLite/tokenization CPU, widening-pass counts and strict live-memory peaks remain unmeasured. SQL scoring work remains corpus dependent; the direct exact scan remains default. macOS, power loss, 100k/one-namespace and million-record scale are unverified in this campaign.

The [rejected iterator experiment](kernel-iterator-experiment.md) preserves initial regression evidence. Final validation: formatting, strict locked all-target/all-feature Clippy, locked tests (51 unit, 12 integration; one ignored diagnostic), locked release build; exhaustive oracle, concurrent keyed writes/conflicts, scope isolation, failed writes, migration rollback, killed-writer ledger rollback and CLI/MCP replay. Earlier intermittent HTTP lock errors and bounded repeat evidence are described in the [kernel notes](kernel-retrieval-persistence.md).
