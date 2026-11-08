# Local embeddings and hybrid retrieval (v0.1 Phase 2)

Midnight can add semantic retrieval on top of its existing FTS5/BM25 search. It is **opt-in, local,
and never required**: every lexical command behaves exactly as before and never loads the model.

## Model

| | |
| --- | --- |
| Name | `multilingual-e5-small` (`intfloat/multilingual-e5-small`, MIT) |
| Dimensions | 384, L2-normalised, cosine = dot product |
| Input | `query:` / `passage:` prefixes applied automatically; tokenizer truncates at 512 tokens |
| Runtime | FastEmbed-rs `=5.13.0` over ONNX Runtime, CPU provider only |

Files live in `AETHER_MIDNIGHT_MODELS_DIR` (default `<OS app-data>/aether-midnight/models`) in the
Hugging Face cache layout. Inference reads those files only; there is no network access outside
`models pull`.

## Commands

```sh
aether-midnight models list                      # installed? revision?
aether-midnight models pull multilingual-e5-small # the ONLY command that uses the network (~470 MB)

aether-midnight memory add --namespace project:app "El servidor usa caché de disco"
aether-midnight embed --namespace project:app     # incremental; safe to interrupt (exit 130)
aether-midnight index --repo . && aether-midnight embed --repo .

aether-midnight memory search --namespace project:app "disk cache" --mode hybrid   # lexical | semantic | hybrid
aether-midnight search "where do we validate passwords" --mode hybrid --repo .
```

`--mode` defaults to `lexical`, which keeps the legacy output byte-for-byte (a bare array for
`memory search`, the original object for `search`). `semantic` and `hybrid` return the same result
objects wrapped with status fields:

```json
{ "mode_requested": "hybrid", "mode_used": "hybrid", "degraded": false, "notice": null,
  "model": {"name":"multilingual-e5-small","revision":"…","dimensions":384},
  "semantic_coverage": {"eligible": 120, "embedded": 120}, "fusion": "rrf(k=60)",
  "results": [ … ] }
```

For code, the original `CodeSearchResponse` fields (`results`, `token_budget`, …) are flattened beside
these. Each result keeps its source path, line range, snippet, freshness and scope.

### Degraded behaviour

If a semantic stage cannot run (model not installed, ONNX Runtime missing, no vectors stored for the
scope, vectors from a different model revision, inference error) the command still succeeds with
lexical results, `degraded: true`, `mode_used: "lexical"`, a `notice` with the reason, and a
`warning:` line on stderr. Partial coverage (some items not yet embedded) is reported in `notice` and
`semantic_coverage`. When SQLite has no vectors for the scope the model is not even loaded.

## Storage (schema 6)

One table, `embeddings(kind, scope, owner_id, chunk, line_start, line_end, model, revision, dims,
source_hash, vector)` with primary key `(kind, owner_id, chunk)`; vectors are little-endian `f32`
BLOBs (1,536 bytes each).

* `kind='memory'`: `scope` = namespace, `owner_id` = `memories.id`, `source_hash` = SHA-256 of content.
* `kind='code'`: `scope` = `repo_id:worktree_id`, `owner_id` = `code_files.id`, one row per
  line-window chunk (≤40 lines / ≤2,000 bytes, ≤400 chunks per file), `source_hash` covers the file
  hash and chunk text.

Triggers delete a memory's vector when the memory is deleted or forgotten, and a file's vectors when
its row is deleted or its `content_hash` changes. `embed` additionally removes vectors for missing
owners and for any other model/revision, and skips unchanged items by hash.

## Retrieval

* **Exact KNN**: all vectors for `(kind, scope, model, revision, dims)` are read and scored with a
  dot product; ties break by ascending id/path. No ANN. sqlite-vec was not adopted: exact scans are
  adequate at the supported scale and it would add a native extension to every release build.
* **Isolation is in the query**: scope, model identity and a join to the live source row (not
  deleted, valid at query time, same namespace / repo+worktree) are SQL predicates, evaluated before
  any vector is read. Code and memory vectors are separate `kind`s.
* **Fusion**: Reciprocal Rank Fusion, `score = Σ 1/(60 + rank)` over the lexical top-50 (code: 20)
  and semantic top-50 (code: best chunk per file, top 20). Equal scores order by ascending id/path,
  so output is deterministic. `match_reason` records `hybrid:lexical#r+semantic#r(cos …)`.
* **Freshness**: a semantic code hit is returned only if the file still hashes to the indexed value.
* Lexical retrieval code, candidate generation and ranking are unchanged.

## ONNX Runtime

`ort` has no prebuilt binaries for every target (none for macOS x86_64), so the runtime is loaded
dynamically. Set `ORT_DYLIB_PATH` or put `libonnxruntime.{dylib,so}` / `onnxruntime.dll` in
`<models dir>/onnxruntime/`, `/opt/homebrew/lib`, `/usr/local/lib` or `/usr/lib`. FastEmbed 5.13 /
`ort` rc.11 needs ONNX Runtime ≥ 1.22 (1.23.2 verified). A missing library is an error, not a crash,
and lexical commands never look for it.

## Privacy

Vectors are derived from stored text and are stored in the same local database (`0600`). They are
removed with their source rows, but, like memory content, may linger in SQLite pages until `compact`.
