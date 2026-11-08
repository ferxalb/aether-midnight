# Real previous-schema databases

These databases were produced by **actual earlier builds** of this project, not hand-written SQL.
Each was created by building the listed source revision in release mode (`git archive <rev>`),
then running that build's own CLI against a fresh database:

| File | Schema (`user_version`) | Source revision | Built from |
| --- | --- | --- | --- |
| `schema_v3.sqlite3` | 3 | `aef6282` (parent of `e53085b`) | `git archive aef6282` |
| `schema_v4.sqlite3` | 4 | `5494559` (parent of `9653863`) | `git archive 5494559` |
| `schema_v5.sqlite3` | 5 | `2177a79` (Phase 1 head) | `git archive 2177a79` |

Commands (old CLI syntax, synthetic data only):

```
remember --scope proj:a --content "alpha zebra memory from old schema"
remember --scope proj:a --content "beta giraffe memory to be forgotten"
remember --scope proj:b --content "gamma zebra memory in another namespace"
forget --scope proj:a --id 2
index --repo <two-file synthetic repo>     # src/lib.rs, README.md
compact                                    # checkpoint WAL into one file
```

The only post-processing: `UPDATE repositories SET root='/fixture/repo'` (removes a local temp
path) and a WAL checkpoint. `tests/migration_fixtures.rs` migrates each file with the current binary.
