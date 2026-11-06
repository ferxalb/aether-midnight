# Rejected repeated-iterator classifier experiment

Baseline `1f30289`, candidate working tree, identical release settings, same host, sequential runs without compilation. 10,000 records; warm persistent connections; shared-host measurements. No latency outliers removed. WAL/NORMAL unchanged. Raw output stays ignored. Frozen suites: all 83 queries retain every quality metric, first-relevant rank and full top-10 list; zero namespace leaks or temporal-invalid results. One evaluation repeat per suite.

## Zipf retrieval

30 samples per strategy/class/layout. Production staged path only below. Latencies are p50/p95/p99 ms; CPU is mean ms/query.

| Namespaces/class | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| 1/broad_ubiquitous_single_term | 4.790/12.144/27.334 | 4.632/5.465/5.880 | 6.154 → 4.706 |
| 1/broad_mid_single_term | 1.318/1.720/1.872 | 1.190/1.454/1.757 | 1.381 → 1.238 |
| 1/selective_single_term | 0.162/0.410/0.450 | 0.129/0.267/0.281 | 0.205 → 0.143 |
| 1/two_heavy_terms | 1.725/2.244/2.385 | 1.562/1.791/1.820 | 1.803 → 1.567 |
| 1/heavy_plus_rare | 0.514/0.820/0.984 | 0.436/0.532/0.574 | 0.549 → 0.444 |
| 1/three_mixed_terms | 0.812/1.271/1.353 | 0.624/0.758/0.989 | 0.825 → 0.640 |
| 1/planted_contiguous_phrase | 2.010/3.026/4.041 | 2.108/7.757/14.279 | 2.142 → 2.641 |
| 1/natural_language_with_stop_words | 2.257/2.642/6.382 | 2.007/2.258/2.322 | 2.396 → 2.063 |

1 namespaces: peak RSS MiB 9.96 → 9.34; database bytes 4444160 → 4448256.

| Namespaces/class | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| 4/broad_ubiquitous_single_term | 1.986/2.444/2.772 | 2.016/9.313/10.209 | 2.038 → 2.683 |
| 4/broad_mid_single_term | 0.758/0.952/0.972 | 0.725/0.898/2.903 | 0.775 → 0.826 |
| 4/selective_single_term | 0.247/0.851/3.522 | 0.246/0.398/1.004 | 0.392 → 0.285 |
| 4/two_heavy_terms | 1.591/2.094/2.176 | 1.725/2.303/2.782 | 1.638 → 1.821 |
| 4/heavy_plus_rare | 0.618/0.901/1.852 | 0.660/2.499/2.701 | 0.689 → 0.825 |
| 4/three_mixed_terms | 0.887/1.193/1.235 | 0.902/1.413/1.631 | 0.898 → 0.940 |
| 4/planted_contiguous_phrase | 2.226/5.911/9.384 | 2.222/2.959/3.245 | 2.829 → 2.291 |
| 4/natural_language_with_stop_words | 2.546/2.997/3.128 | 2.169/2.641/2.731 | 2.542 → 2.217 |

4 namespaces: peak RSS MiB 10.21 → 9.24; database bytes 4415488 → 4419584.

## Tied retrieval

30 samples/path, interleaved direct/descending/production controls; production comparison below.

| Namespaces | Baseline latency | Candidate latency | CPU baseline → candidate |
|---|---|---|---|
| 1 | 6.893/9.683/11.370 | 6.203/7.071/8.709 | 7.204 → 6.282 |
| 4 | 2.351/2.649/2.875 | 2.351/3.031/3.207 | 2.385 → 2.399 |

## Memory workload

100 recall samples and 30 write samples/layout. CPU covers setup, writes and profiling, not CPU/query. RSS is whole-process peak.

| Namespaces | Recall baseline → candidate | Write baseline → candidate | CPU seconds | Peak RSS MiB |
|---|---|---|---|---|
| 1 | 7.419/10.346/11.663 → 7.006/9.450/10.418 | 0.544/1.035/1.307 → 0.605/1.046/1.349 | 9.614 → 9.458 | 14.11 → 13.28 |
| 4 | 2.736/3.179/5.902 → 2.794/10.043/20.457 | 0.508/0.751/1.315 → 0.542/0.648/1.098 | 4.131 → 3.993 | 14.04 → 12.84 |

These results include slower classes and tails. They do not establish a universal CPU or latency gain. The implemented allocation-site reduction is limited to production candidate classification; historical benchmark strategies remain unchanged. Allocator-wide counts, isolated staged SQLite versus tokenization CPU, and strict live-memory peaks remain unmeasured. Candidate SQL work remains corpus dependent and no production scan strategy changed. No failed scan-order experiment was integrated. macOS, power loss and million-record scale remain unverified.

This is diagnostic evidence for the initial classifier. It was replaced by borrowed token descriptors after mixed CPU and tail results. These numbers do not describe the delivered classifier.
