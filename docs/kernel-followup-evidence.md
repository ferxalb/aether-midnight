# Kernel follow-up: Linux aggregate evidence

All timings are milliseconds. p50/p95/p99 use the existing nearest-index ceiling rule. No outliers are removed. Original 30-sample p99 values were sample maxima; the new comparisons use identical counts per path, rotated order and warm persistent connections. No compilation overlapped measurement. Raw per-call output remains ignored. These shared-host observations do not establish a host-independent latency bound.

The historical candidate has now been identified precisely: a clean rebuild of local pre-publication commit `544d6f4` matches the retained benchmark binary byte-for-byte. The `1f30289` baseline likewise matches its retained binary. Published `9653863` differs in HTTP/WAL setup, although its retrieval source is byte-identical to `544d6f4`. The tables below distinguish those artifacts; the fixed-namespace section separately checks the exact historical candidate.

## Published-commit comparisons

Baseline `1f30289` and published candidate `9653863`, each built with identical locked release settings. Original baseline is run twice as an identical-code control. Each cell is p50/p95/p99. Both trials use 100k records/four namespaces, 200 calls/path/query; full decoded results match every sample. CPU is captured in the separate controlled ablations below, not inferred from RPC wall time.

Trial 1.

| Query | Baseline | Identical control | Published candidate |
|---|---|---|---|
| w0 | 19.41/24.61/34.63 | 19.34/23.58/26.77 | 19.09/23.46/25.94 |
| w10 | 6.53/8.95/15.12 | 6.35/8.03/9.64 | 6.26/8.27/10.87 |
| w2000 | 1.28/1.55/1.73 | 1.22/1.50/1.87 | 1.20/1.46/1.59 |
| w3 w5 | 13.13/15.24/18.39 | 12.90/15.08/18.50 | 12.94/15.05/17.08 |
| w10 w2000 | 2.82/4.77/6.10 | 2.79/4.82/5.00 | 2.89/5.03/6.63 |
| w50 w300 w2000 | 3.37/4.65/10.27 | 3.38/4.39/6.35 | 3.28/4.73/7.37 |
| w98 w2928 w113 | 3.67/5.46/6.95 | 3.56/5.16/7.59 | 3.39/5.05/6.73 |
| what is the w98 w2928 w113 and w2000 | 3.94/4.87/9.17 | 3.90/4.91/8.10 | 3.69/4.56/6.23 |

Trial 2.

| Query | Baseline | Identical control | Published candidate |
|---|---|---|---|
| w0 | 19.41/23.14/34.07 | 18.91/22.59/24.69 | 18.44/21.49/26.62 |
| w10 | 6.43/7.91/10.03 | 6.08/7.23/11.74 | 6.29/8.17/11.14 |
| w2000 | 1.31/1.79/2.21 | 1.31/1.80/2.28 | 1.29/1.82/2.60 |
| w3 w5 | 14.13/22.61/25.83 | 13.41/16.84/22.97 | 13.55/18.03/24.02 |
| w10 w2000 | 2.90/5.52/7.65 | 3.20/5.62/7.09 | 3.11/5.67/6.96 |
| w50 w300 w2000 | 3.47/4.74/6.19 | 3.43/5.19/6.05 | 3.37/5.19/5.96 |
| w98 w2928 w113 | 3.85/5.84/15.77 | 3.63/5.29/6.37 | 3.47/5.46/9.93 |
| what is the w98 w2928 w113 and w2000 | 4.43/6.14/8.88 | 4.42/5.81/16.12 | 4.10/5.79/8.59 |

The original broad-query 22.47→39.41 ms p99 difference did not reproduce in either actual-commit trial. The baseline duplicate itself has materially different tails. This does not identify the original stall as scheduling or physical I/O.

## Reference-path snapshot comparisons

Restored retrieval snapshot versus `1f30289`, with a second baseline control, same 100k/four-namespace fixture and 200 calls/path/query in each of two independent runs. Full decoded response equality is asserted on every sample. Each cell is p50/p95/p99; child process CPU ms/query is shown separately, quantized by the Linux process accounting clock. Per-call thread CPU is measured in the in-process diagnostic instead.

Trial 1: per-child peak RSS MiB, baseline/control/restored = 8.82/8.82/8.79.

| Query | Baseline | Identical control | Restored | CPU baseline/control/restored |
|---|---|---|---|---|
| w0 | 19.05/23.48/32.82 | 19.49/25.28/42.03 | 18.68/23.50/26.22 | 19.55/20.30/19.15 |
| w10 | 6.15/6.76/24.18 | 6.08/7.02/9.46 | 6.05/7.08/27.49 | 6.30/6.20/6.50 |
| w2000 | 1.28/1.78/2.14 | 1.32/1.72/3.90 | 1.30/1.59/2.11 | 1.30/1.35/1.25 |
| w3 w5 | 13.48/16.55/23.35 | 13.21/17.17/23.96 | 14.03/18.43/29.16 | 13.80/13.60/14.35 |
| w10 w2000 | 2.97/5.20/7.45 | 2.78/4.82/5.73 | 3.11/5.40/9.00 | 3.30/3.15/3.45 |
| w50 w300 w2000 | 3.26/4.09/6.72 | 3.19/3.96/19.57 | 3.28/4.00/5.73 | 3.35/3.55/3.35 |
| w98 w2928 w113 | 3.75/5.57/7.18 | 3.51/5.25/7.00 | 3.66/5.39/6.51 | 3.90/3.70/3.90 |
| what is the w98 w2928 w113 and w2000 | 4.07/5.14/6.76 | 4.08/4.91/6.89 | 3.82/4.81/7.45 | 4.10/4.05/3.95 |

Trial 2: per-child peak RSS MiB, baseline/control/restored = 8.76/8.80/8.87.

| Query | Baseline | Identical control | Restored | CPU baseline/control/restored |
|---|---|---|---|---|
| w0 | 18.69/22.42/24.09 | 18.66/21.56/25.89 | 19.03/21.91/26.72 | 18.85/18.80/19.15 |
| w10 | 6.35/8.47/13.48 | 6.47/8.45/13.75 | 6.34/9.00/12.73 | 6.55/6.70/6.60 |
| w2000 | 1.28/1.51/2.36 | 1.27/1.52/1.73 | 1.28/1.51/1.57 | 1.25/1.25/1.25 |
| w3 w5 | 13.28/16.39/22.03 | 13.18/15.12/18.52 | 13.27/15.40/18.41 | 13.55/13.30/13.50 |
| w10 w2000 | 2.83/4.76/5.61 | 2.81/4.67/5.11 | 2.80/4.71/5.28 | 3.15/3.10/3.10 |
| w50 w300 w2000 | 3.29/4.50/7.70 | 3.28/4.62/6.29 | 3.30/4.81/7.64 | 3.40/3.40/3.40 |
| w98 w2928 w113 | 3.44/4.88/6.66 | 3.49/4.90/6.92 | 3.48/5.26/7.19 | 3.65/3.65/3.75 |
| what is the w98 w2928 w113 and w2000 | 3.90/4.61/5.73 | 3.91/4.68/5.56 | 3.75/4.46/5.91 | 3.90/3.95/3.80 |

Slower delivered classes are included rather than discarded. These data support restoration of the original retrieval algorithm and show variability even with identical code; they do not establish that every tail or small CPU difference has a known cause. No universal p99 improvement is claimed.

## Controlled ablations

Persistence, SQL, corpus, cache settings and connection are constant. Baseline and control select the original path; combined selects all three former optimizations. The remaining paths independently enable borrowed tokens, ASCII normalization or deferred hydration. Cells are p50/p95/p99; mean thread CPU ms/query. Work diagnostics run separately after acceptance timing. Shared process RSS includes accumulated diagnostic report buffers and cannot be assigned to an individual path.

100k/four namespaces, trial 1, 200 calls/path/class. Shared peak RSS 28.39 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| broad_ubiquitous_single_term | 17.79/20.89/23.29; 18.16 | 17.64/21.07/30.77; 18.31 | 17.57/20.64/23.06; 18.17 | 17.59/21.24/26.27; 18.17 | 17.71/21.43/25.85; 18.15 | 17.69/21.12/29.09; 18.57 |
| broad_mid_single_term | 5.56/6.82/9.51; 5.77 | 5.50/6.69/9.96; 5.76 | 5.54/6.50/8.55; 5.68 | 5.50/6.47/7.04; 5.62 | 5.53/6.48/6.89; 5.65 | 5.53/6.47/9.64; 5.71 |
| selective_single_term | 1.00/1.24/1.43; 1.05 | 1.02/1.32/1.68; 1.06 | 0.99/1.23/2.02; 1.03 | 0.99/1.28/1.99; 1.03 | 0.99/1.25/1.37; 1.03 | 1.01/1.26/1.67; 1.04 |
| two_heavy_terms | 12.09/13.75/14.63; 12.31 | 12.12/14.39/17.95; 12.33 | 12.12/13.65/17.71; 12.24 | 12.05/14.02/16.05; 12.25 | 12.15/13.69/17.57; 12.26 | 12.13/13.88/15.91; 12.28 |
| heavy_plus_rare | 2.40/4.27/4.94; 2.77 | 2.35/4.19/4.96; 2.77 | 2.39/4.07/4.37; 2.72 | 2.36/4.13/4.63; 2.74 | 2.36/4.24/4.96; 2.74 | 2.42/4.35/5.89; 2.81 |
| three_mixed_terms | 2.76/3.30/4.09; 2.85 | 2.80/3.39/4.35; 2.87 | 2.75/3.50/6.00; 2.88 | 2.79/3.51/6.09; 2.90 | 2.78/3.65/4.63; 2.94 | 2.80/3.45/5.65; 2.91 |
| planted_contiguous_phrase | 3.10/4.63/7.08; 3.31 | 3.10/4.59/7.48; 3.46 | 3.00/4.26/5.83; 3.24 | 3.02/4.84/5.82; 3.30 | 3.05/4.67/7.78; 3.49 | 3.08/4.37/5.15; 3.28 |
| natural_language_with_stop_words | 3.65/4.92/7.02; 3.82 | 3.68/5.55/8.11; 3.87 | 3.54/4.32/6.05; 3.66 | 3.61/4.46/5.45; 3.67 | 3.60/4.66/8.78; 3.74 | 3.71/4.78/11.82; 3.84 |

100k/four namespaces, trial 2, 200 calls/path/class. Shared peak RSS 28.14 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| broad_ubiquitous_single_term | 18.80/21.42/26.25; 18.91 | 18.69/21.16/26.38; 18.78 | 18.64/21.12/26.06; 18.99 | 18.73/24.15/43.26; 19.44 | 18.68/22.78/26.22; 19.21 | 18.60/23.47/38.04; 19.18 |
| broad_mid_single_term | 5.46/8.59/19.64; 5.94 | 5.48/6.95/9.52; 5.74 | 5.48/7.27/10.09; 5.70 | 5.44/7.38/19.79; 5.90 | 5.47/7.18/15.13; 5.76 | 5.51/7.47/9.72; 5.75 |
| selective_single_term | 1.06/1.43/1.73; 1.10 | 1.09/1.37/1.49; 1.11 | 1.07/1.35/1.48; 1.09 | 1.06/1.38/1.68; 1.10 | 1.06/1.38/1.57; 1.10 | 1.09/1.38/1.70; 1.12 |
| two_heavy_terms | 12.55/16.66/20.71; 12.99 | 12.52/15.38/32.90; 13.14 | 12.49/17.80/50.17; 13.51 | 12.49/16.10/26.12; 13.07 | 12.49/15.95/45.16; 13.21 | 12.51/15.65/26.43; 13.09 |
| heavy_plus_rare | 2.38/4.37/7.40; 2.87 | 2.39/4.28/6.35; 2.81 | 2.41/4.40/6.04; 2.79 | 2.38/4.59/6.16; 2.81 | 2.38/4.34/5.40; 2.79 | 2.41/4.41/6.85; 2.86 |
| three_mixed_terms | 2.68/3.25/3.73; 2.74 | 2.70/3.19/4.95; 2.77 | 2.66/3.06/3.75; 2.70 | 2.68/3.11/4.73; 2.77 | 2.69/3.09/3.64; 2.73 | 2.71/3.21/6.38; 2.80 |
| planted_contiguous_phrase | 2.93/4.81/7.92; 3.27 | 2.90/4.68/7.60; 3.25 | 2.90/4.49/5.28; 3.15 | 2.89/4.68/5.83; 3.18 | 2.89/4.55/6.80; 3.18 | 2.94/4.57/5.64; 3.20 |
| natural_language_with_stop_words | 3.61/4.55/5.36; 3.67 | 3.53/4.76/12.88; 3.79 | 3.50/4.27/5.27; 3.59 | 3.50/4.35/7.81; 3.59 | 3.50/4.24/9.13; 3.64 | 3.53/4.37/7.10; 3.65 |

10k/1 namespaces, tied=False, 100 calls/path/class. Shared peak RSS 17.49 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| broad_ubiquitous_single_term | 4.60/5.83/6.44; 4.69 | 4.55/5.94/11.59; 4.76 | 4.55/5.84/10.23; 4.71 | 4.55/5.92/11.76; 4.77 | 4.55/5.55/10.42; 4.67 | 4.59/6.04/11.19; 4.80 |
| broad_mid_single_term | 1.22/1.49/1.99; 1.26 | 1.22/1.48/2.20; 1.27 | 1.20/1.59/2.60; 1.26 | 1.20/1.58/3.15; 1.27 | 1.22/1.51/2.09; 1.27 | 1.23/1.52/1.63; 1.27 |
| selective_single_term | 0.12/0.18/0.26; 0.13 | 0.12/0.20/0.33; 0.13 | 0.09/0.15/0.24; 0.10 | 0.11/0.15/0.29; 0.12 | 0.10/0.20/0.27; 0.12 | 0.12/0.18/0.29; 0.13 |
| two_heavy_terms | 1.44/1.78/8.93; 1.56 | 1.44/1.88/2.83; 1.51 | 1.43/1.91/2.99; 1.51 | 1.44/1.97/7.30; 1.59 | 1.43/2.09/9.27; 1.66 | 1.44/1.87/5.42; 1.52 |
| heavy_plus_rare | 0.40/0.52/0.65; 0.41 | 0.40/0.52/0.79; 0.41 | 0.39/0.48/0.92; 0.41 | 0.40/0.51/0.68; 0.41 | 0.40/0.49/1.22; 0.41 | 0.41/0.58/0.65; 0.42 |
| three_mixed_terms | 0.62/0.86/1.76; 0.66 | 0.62/0.79/2.25; 0.65 | 0.60/0.92/1.10; 0.65 | 0.61/0.79/0.97; 0.64 | 0.61/0.82/1.19; 0.64 | 0.61/0.91/0.96; 0.65 |
| planted_contiguous_phrase | 1.92/2.38/8.73; 2.01 | 1.95/2.46/6.76; 2.03 | 1.84/2.63/8.52; 1.98 | 1.91/2.43/6.63; 2.02 | 1.87/3.09/18.84; 2.18 | 1.97/2.40/3.99; 2.01 |
| natural_language_with_stop_words | 2.35/4.61/5.96; 2.59 | 2.39/4.09/5.73; 2.56 | 2.26/3.60/4.61; 2.40 | 2.28/3.91/6.99; 2.49 | 2.28/4.08/4.75; 2.45 | 2.34/3.81/27.82; 2.67 |

10k/1 namespaces, tied=True, 100 calls/path/class. Shared peak RSS 9.06 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| tied_single_term | 6.74/8.47/23.34; 7.00 | 6.55/8.95/15.90; 6.87 | 6.52/8.53/26.28; 6.94 | 6.55/9.29/15.91; 6.81 | 6.60/8.44/18.19; 6.76 | 6.69/10.03/19.46; 6.97 |

10k/4 namespaces, tied=False, 100 calls/path/class. Shared peak RSS 17.75 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| broad_ubiquitous_single_term | 2.14/3.18/4.62; 2.20 | 2.10/2.86/4.16; 2.19 | 2.10/3.32/4.46; 2.21 | 2.09/3.61/7.28; 2.24 | 2.06/2.85/4.78; 2.19 | 2.16/3.49/3.94; 2.29 |
| broad_mid_single_term | 0.81/1.26/2.11; 0.88 | 0.83/1.64/3.69; 0.95 | 0.81/1.66/3.21; 0.94 | 0.80/1.94/2.52; 0.91 | 0.78/2.83/4.37; 0.97 | 0.80/1.34/3.86; 0.91 |
| selective_single_term | 0.23/0.82/1.44; 0.29 | 0.23/0.40/0.86; 0.25 | 0.24/0.40/1.81; 0.26 | 0.24/0.64/2.23; 0.28 | 0.23/0.43/1.33; 0.25 | 0.24/0.42/1.46; 0.25 |
| two_heavy_terms | 1.96/3.32/6.24; 2.11 | 1.93/3.09/6.45; 2.13 | 1.89/3.24/11.35; 2.07 | 1.87/2.83/10.49; 2.03 | 1.95/3.65/10.39; 2.17 | 1.85/3.51/9.84; 2.11 |
| heavy_plus_rare | 0.65/1.00/1.32; 0.67 | 0.62/1.14/2.13; 0.69 | 0.67/1.07/4.15; 0.73 | 0.64/1.30/3.67; 0.73 | 0.65/1.12/2.68; 0.70 | 0.65/1.34/4.05; 0.75 |
| three_mixed_terms | 0.82/1.84/6.95; 0.98 | 0.81/1.57/2.77; 0.89 | 0.81/1.30/3.46; 0.87 | 0.83/1.78/3.08; 0.93 | 0.81/2.02/7.32; 1.01 | 0.89/3.42/7.25; 1.15 |
| planted_contiguous_phrase | 1.90/3.40/4.19; 2.08 | 1.96/3.57/4.35; 2.07 | 1.84/4.01/27.09; 2.28 | 1.95/3.17/3.96; 2.08 | 1.89/3.20/9.07; 2.05 | 1.89/3.43/5.05; 2.08 |
| natural_language_with_stop_words | 2.25/3.15/5.34; 2.34 | 2.23/3.46/4.88; 2.36 | 2.22/3.66/31.79; 2.58 | 2.20/4.01/14.54; 2.43 | 2.23/3.77/5.39; 2.40 | 2.22/3.30/5.30; 2.40 |

10k/4 namespaces, tied=True, 100 calls/path/class. Shared peak RSS 9.24 MiB.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| tied_single_term | 2.37/4.96/7.43; 2.63 | 2.41/3.65/5.45; 2.57 | 2.35/3.77/5.08; 2.50 | 2.38/4.06/31.12; 2.82 | 2.37/3.90/4.31; 2.48 | 2.41/4.24/7.11; 2.62 |

The combined and individual paths have occasional slower tails, including the second 100k heavy-term combined p99. Effects are not stable across trials and controls. No experiment is credited with fixing a reproducible broad-tail defect. Production restores the reference path; allocation-site reductions and the smaller tokenization CPU are intentionally given back rather than claimed as a proven full-path tail win. The optimizations remain diagnostic-only.

## Allocation and candidate work

Separate allocator build, 100k/four namespaces, 10 diagnostic calls/path/class. Values are mean Rust allocation/reallocation calls / requested bytes per recall. These are allocation traffic, not live heap. Timings from this binary are excluded from acceptance. SQLite C allocations are not counted. Fixture setup and JSON report serialization are excluded; Rust work-counter bookkeeping is included symmetrically.

| Class | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| broad_ubiquitous_single_term | 425.5/34603.4 | 425.5/34603.4 | 146.0/27854.7 | 186.0/29247.4 | 385.5/33210.7 | 425.5/34603.4 |
| broad_mid_single_term | 389.1/33694.0 | 389.1/33694.0 | 146.2/27178.3 | 186.4/28645.4 | 348.9/32226.9 | 389.1/33694.0 |
| selective_single_term | 403.6/34438.0 | 403.6/34438.0 | 147.2/27641.9 | 188.9/29173.6 | 361.9/32906.3 | 403.6/34438.0 |
| two_heavy_terms | 381.4/30168.0 | 381.4/30168.0 | 175.3/25503.7 | 210.9/26521.5 | 345.8/29150.2 | 381.4/30168.0 |
| heavy_plus_rare | 483.5/40251.0 | 483.5/40251.0 | 174.8/31300.5 | 216.2/33467.9 | 442.1/38083.6 | 483.5/40251.0 |
| three_mixed_terms | 916.6/95711.1 | 916.6/95711.1 | 287.3/75988.8 | 367.8/80832.5 | 836.1/90867.4 | 916.6/95711.1 |
| planted_contiguous_phrase | 1184.5/129844.9 | 1184.5/129844.9 | 363.7/104724.6 | 471.0/110666.2 | 1077.2/123903.3 | 1184.5/129844.9 |
| natural_language_with_stop_words | 1371.9/145691.1 | 1371.9/145691.1 | 440.6/117256.2 | 563.9/124057.2 | 1248.6/138890.1 | 1371.9/145691.1 |

The reduction in transient allocation requests is real, but is not a peak-RSS or universal full-path speed guarantee. Deferred hydration has no allocation benefit on these eligible-winner fixtures. Restoration deliberately returns allocation traffic to reference levels.

Latest work diagnostic, 100k/four namespaces, 100 calls/path/class, final trace implementation. Candidate SQL counts exclude planner probes; outer rows are not internal FTS-scored documents. Bytes are logical hydrated string lengths. Cache counters are not physical disk reads. Baseline/control work should agree for the same namespace/query; all variants preserve results.

| Class/path | Mean SQL executions/widening | Mean rows/classified/rejected/hydrated | Mean content/record bytes | Mean VM steps/sorts |
|---|---|---|---|---|
| broad_ubiquitous_single_term/baseline | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 902.50/1592.75 | 164776.75/1.00 |
| broad_ubiquitous_single_term/control | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 902.50/1592.75 | 164776.75/1.00 |
| broad_ubiquitous_single_term/candidate | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 902.50/1592.75 | 164776.75/1.00 |
| broad_mid_single_term/baseline | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 848.25/1538.50 | 42633.50/1.00 |
| broad_mid_single_term/control | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 848.25/1538.50 | 42633.50/1.00 |
| broad_mid_single_term/candidate | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 848.25/1538.50 | 42633.50/1.00 |
| selective_single_term/baseline | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 898.00/1588.25 | 959.25/1.00 |
| selective_single_term/control | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 898.00/1588.25 | 959.25/1.00 |
| selective_single_term/candidate | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 898.00/1588.25 | 959.25/1.00 |
| two_heavy_terms/baseline | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 653.50/1343.75 | 3242.00/1.00 |
| two_heavy_terms/control | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 653.50/1343.75 | 3242.00/1.00 |
| two_heavy_terms/candidate | 1.00/0.00 | 18.00/11.00/0.00/11.00 | 653.50/1343.75 | 3242.00/1.00 |
| heavy_plus_rare/baseline | 2.00/0.00 | 12.25/9.50/0.00/9.50 | 1146.00/1741.75 | 528.00/2.00 |
| heavy_plus_rare/control | 2.00/0.00 | 12.25/9.50/0.00/9.50 | 1146.00/1741.75 | 528.00/2.00 |
| heavy_plus_rare/candidate | 2.00/0.00 | 12.25/9.50/0.00/9.50 | 1146.00/1741.75 | 528.00/2.00 |
| three_mixed_terms/baseline | 2.00/0.00 | 17.25/17.25/0.00/17.25 | 2329.00/3410.50 | 728.00/2.00 |
| three_mixed_terms/control | 2.00/0.00 | 17.25/17.25/0.00/17.25 | 2329.00/3410.50 | 728.00/2.00 |
| three_mixed_terms/candidate | 2.00/0.00 | 17.25/17.25/0.00/17.25 | 2329.00/3410.50 | 728.00/2.00 |
| planted_contiguous_phrase/baseline | 2.00/0.00 | 23.75/23.50/0.00/23.50 | 2914.50/4388.25 | 988.00/2.00 |
| planted_contiguous_phrase/control | 2.00/0.00 | 23.75/23.50/0.00/23.50 | 2914.50/4388.25 | 988.00/2.00 |
| planted_contiguous_phrase/candidate | 2.00/0.00 | 23.75/23.50/0.00/23.50 | 2914.50/4388.25 | 988.00/2.00 |
| natural_language_with_stop_words/baseline | 2.00/0.00 | 26.50/26.50/0.00/26.50 | 3293.25/4954.50 | 1098.00/2.00 |
| natural_language_with_stop_words/control | 2.00/0.00 | 26.50/26.50/0.00/26.50 | 3293.25/4954.50 | 1098.00/2.00 |
| natural_language_with_stop_words/candidate | 2.00/0.00 | 26.50/26.50/0.00/26.50 | 3293.25/4954.50 | 1098.00/2.00 |

Broad-query acceptance cache deltas and final thread CPU (100 calls/path), same diagnostic binary:

| Path | Mean hits/misses | p50/p95/p99 | Mean thread CPU |
|---|---|---|---|
| baseline | 46918.43/507.07 | 19.12/23.15/28.56 | 19.46 |
| control | 46918.03/507.47 | 18.96/22.48/35.88 | 19.26 |
| candidate | 46918.29/507.21 | 18.64/22.09/27.36 | 19.15 |
| borrowed_only | 46918.03/507.47 | 19.13/23.01/27.47 | 19.33 |
| ascii_only | 46918.43/507.07 | 19.20/25.82/36.74 | 19.79 |
| deferred_only | 46918.19/507.31 | 18.96/22.90/26.04 | 19.25 |

The forced-expiry/cross-namespace widening regression reports fetch limits `[18,72,288,1152]`: four candidate SQL executions, three widening passes, 778 outer rows, 698 filtered rows, 11 classifications and hydrations, zero term rejects, 165 content bytes, 935 record bytes, 40,463 VM steps and four sorts. The exhaustive oracle still agrees. This demonstrates repeated SQL work that final-winner counters alone previously hid; it does not change widening or truncate candidates.

## Standard paired memory workload

Both retained binaries run sequentially on the same host. 10k records, one/four namespaces; standard `bench-memory`, default sample counts. CPU covers the whole workload including setup and writes. Each latency cell is p50/p95/p99 ms.

| Namespaces | Recall baseline → restored | Write baseline → restored | Whole-workload CPU seconds | Peak RSS MiB |
|---|---|---|---|---|
| 1 | 6.64/7.33/8.91 → 6.88/7.87/13.23 | 0.52/0.82/1.17 → 0.59/1.04/9.62 | 1.69 → 1.70 | 9.10 → 9.43 |
| 4 | 2.71/3.32/6.09 → 2.82/4.22/13.65 | 0.54/0.78/1.11 → 0.50/0.72/0.97 | 0.95 → 0.96 | 9.24 → 9.48 |

## Original fixed-namespace reproduction

The original standard retrieval benchmark queried a fixed namespace. Earlier new comparisons rotated namespaces and are identified above. Early probe JSON metadata incorrectly said fixed despite the loop rotating; current metadata is corrected. The following trials explicitly select the original fixed namespace and `w0` on the same deterministic 100k/four-namespace corpus. All decoded response fields match every sample. Each cross-process trial uses 300 calls/path; warm calls, copies and initialization precede timing. CPU is mean child process ms/query, quantized by Linux accounting. RSS is per-child peak MiB.

| Target / trial | Baseline p50/p95/p99 | Identical control | Target | CPU baseline/control/target | RSS baseline/control/target MiB |
|---|---|---|---|---|---|
| Historical 544d6f4 / 1 | 19.20/22.84/42.40 | 18.89/22.84/25.42 | 18.97/24.01/32.74 | 19.73/19.20/19.50 | 8.22/8.26/8.57 |
| Historical 544d6f4 / 2 | 20.46/28.83/36.23 | 19.44/28.72/41.44 | 19.10/27.05/33.49 | 21.47/20.70/19.83 | 8.22/8.23/8.46 |
| Published 9653863 / 1 | 19.70/25.24/42.64 | 19.89/24.99/37.69 | 19.75/25.41/36.68 | 20.50/20.47/20.30 | 8.39/8.41/8.33 |
| Published 9653863 / 2 | 19.92/24.29/52.94 | 19.94/24.05/26.38 | 19.09/23.32/28.82 | 20.73/20.17/19.53 | 8.26/8.34/8.42 |
| Delivered reference / 1 | 19.95/25.98/46.24 | 18.96/26.42/35.17 | 18.92/24.99/32.34 | 20.67/19.93/19.50 | 8.45/8.43/8.68 |
| Delivered reference / 2 | 19.70/24.77/30.38 | 19.56/24.75/28.19 | 19.01/23.33/30.23 | 20.20/19.87/19.57 | 8.43/8.36/8.61 |

In-process controlled fixed-namespace ablations: 200 calls/path/trial, one connection and constant persistence implementation. Each cell is p50/p95/p99; mean thread CPU ms/query. The uninstrumented acceptance pass is distinct from work/allocator diagnostics.

| Trial | Baseline | Identical control | Combined | Borrowed only | ASCII only | Deferred only |
|---|---|---|---|---|---|---|
| 1 | 17.73/20.61/24.68; 17.96 | 17.79/21.09/26.36; 18.21 | 17.75/21.10/23.11; 18.02 | 17.72/20.34/24.33; 18.10 | 17.71/22.35/26.46; 18.13 | 17.74/21.50/24.06; 17.99 |
| 2 | 17.60/20.84/25.96; 17.94 | 17.67/20.86/22.50; 17.97 | 17.70/21.52/43.09; 18.31 | 17.71/21.12/23.26; 18.11 | 17.71/20.90/28.35; 18.17 | 17.61/20.14/24.43; 18.28 |

The combined path has a candidate-only 43.09 ms p99 in the second fixed-namespace trial, versus 25.96 reference and 22.50 control. In the first trial it is 23.11 versus 24.68/26.36. The p99-defining combined call in trial two used 43.07 ms thread CPU and had 507 SQLite cache misses; reference used 21.42 ms CPU at its 25.96 ms p99 call, also with 507 misses. This is evidence of a CPU-heavy tail sample, not proof of a particular optimization, scheduler effect or physical disk activity. The historical actual-binary trials do not show a stable candidate-only gap. The combined optimizations therefore do not clear the strict tail gate and remain isolated rather than being credited with a fix.

Delivered reference-path p99 is 32.34 and 30.23 ms in the fixed trials; baseline is 46.24/30.38 and identical control 35.17/28.19. No unexplained repeatable broad CPU or process-RSS increase is demonstrated in those matched trials. The original retrieval implementation is restored; neither original tail causality nor a universal improvement is claimed. HTTP initialization is completed and closed before readiness, preserving request close/checkpoint behavior.

## Validation and limits

All 83 frozen queries retain every non-latency field, including quality metrics, first relevant rank, full top-10, response bytes and context estimates. Namespace leaks and temporal-invalid results remain zero. Exhaustive oracle tests run on production and all eight diagnostic flag combinations; targeted Unicode/phrase tests do likewise. Default locked tests, diagnostic-feature locked tests, formatting, strict all-target/all-feature Clippy and locked release build pass. Tests retain keyed concurrency/conflicts, scope isolation, failed writes, maintenance concurrency, migration rollback and killed-writer recovery. Ten repeated HTTP suites pass after bootstrap ordering is fixed. The warm-database journal probe separately records zero failures for 4,000 setter and 4,000 query-only calls; it did not reproduce the cold bootstrap race.

Linux is measured. Native macOS is unavailable/unverified. Process-crash recovery is tested; power-loss durability is not. WAL/NORMAL, cache limits, candidate limits and result ordering are unchanged. Exact broad-query FTS scoring remains expensive. The cause of the original individual tail samples is not established; a cross-host or maximum-latency guarantee is not claimed.

An additional `cargo test --locked --all-features` attempt could not link the pre-existing optional scan-status feature without SQLite's `SQLITE_ENABLE_STMT_SCANSTATUS` build flag. It is not counted as a pass. Required default tests, selected diagnostic-feature tests and strict all-feature Clippy passed; scan-status is not needed for the statement/cache counters reported here.
