# Embeddings and hybrid retrieval (v0.1 Phases 2–3)

Midnight can add semantic retrieval on top of its existing FTS5/BM25 search. It is **opt-in and
never required**: lexical commands never load a model, the ONNX
Runtime, or a network client. Embeddings come from the local model (default) or, only when you
select it explicitly, a cloud provider (see [Cloud providers](#cloud-providers)).

## Model

| | |
| --- | --- |
| Name | `multilingual-e5-small` (`intfloat/multilingual-e5-small`, MIT) |
| Pinned revision | `614241f622f53c4eeff9890bdc4f31cfecc418b3` (Hugging Face commit; never `main`) |
| Dimensions | 384, L2-normalised, cosine = dot product |
| Input | `query:` / `passage:` prefixes applied automatically; tokenizer truncates at 512 tokens |
| Runtime | FastEmbed-rs `=5.13.0` over ONNX Runtime, CPU provider only |

Files live in `AETHER_MIDNIGHT_MODELS_DIR` (default `<OS app-data>/aether-midnight/models`) in the
Hugging Face cache layout. Inference reads those files only; the only network access is the explicit
`models pull` and `runtime install`.

### Integrity

`models pull` downloads exactly the pinned revision and then checks all five files (size and SHA-256)
against pins compiled into the binary (`MODEL_FILE_PINS` in `src/embed.rs`; the two large files'
hashes are the Hugging Face LFS object ids). Anything else is rejected:

* `models list` accepts only the pinned snapshot, by exact byte size; other cached revisions are
  ignored and reported as `ignored_revisions`.
* `models verify` hashes every file offline. `doctor` runs the same check.
* Loading the model hashes each file in memory before it reaches the tokenizer or ONNX Runtime, so a
  file modified after `pull` cannot be used; semantic commands then degrade with the reason.

Bumping the pin is a code change that must update the table, `docs/v0.1-status.md` and the licence
inventory. Vectors stored under another revision string are never mixed with the new one (see
[Embedding-space isolation](#embedding-space-isolation)).

## Commands

```sh
aether-midnight runtime install                  # explicit, SHA-256-verified ONNX Runtime (macOS x86_64)
aether-midnight models pull multilingual-e5-small # explicit, pinned + verified (~470 MB)
aether-midnight models list                      # installed? pinned revision? providers?
aether-midnight models verify                    # hash every model file against its pin (offline)
aether-midnight runtime status                   # path, version, compatibility, checksum; exit 1 if unusable
aether-midnight doctor                           # database + optional runtime/model/cloud diagnostics

aether-midnight memory add --namespace project:app "El servidor usa caché de disco"
aether-midnight embed --namespace project:app     # incremental; safe to interrupt (exit 130)
aether-midnight index --repo . && aether-midnight embed --repo .

aether-midnight memory search --namespace project:app "disk cache" --mode hybrid   # lexical | semantic | hybrid
aether-midnight search "where do we validate passwords" --mode hybrid --repo .
```

`memory search` defaults to `lexical`. Code `search` and MCP stdio `search_code` default to
`hybrid` with a saved provider and otherwise `lexical`; HTTP also checks token permissions.
Lexical keeps the bare memory array or code response shape. Code literal-phrase ordering has
changed in rc.6; see [the ranking contract](hybrid-mcp.md). `semantic` and `hybrid` return result
objects wrapped with status fields:

```json
{ "mode_requested": "hybrid", "mode_used": "hybrid", "degraded": false, "notice": null,
  "model": {"name":"multilingual-e5-small","revision":"…","dimensions":384},
  "semantic_coverage": {"eligible": 120, "embedded": 120}, "fusion": "rrf(k=60)",
  "results": [ … ] }
```

For code, the original `CodeSearchResponse` fields (`results`, `token_budget`, …) are flattened beside
these. Each result keeps its source path, line range, snippet and freshness; the scope is stated once on the response.

### Degraded behaviour

If a semantic stage cannot run (model not installed, ONNX Runtime missing, no vectors stored for the
scope, vectors from a different model revision, inference error) the command still succeeds with
lexical results, `degraded: true`, `mode_used: "lexical"`, a `notice` with the reason, and a
`warning:` line on stderr. Partial coverage (some items not yet embedded) is reported in `notice` and
`semantic_coverage`. When SQLite has no vectors for the scope the model is not even loaded.

## Storage (schema 6, extended in 7)

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
* **Code literal evidence (v0.1.0-rc.6)**: a separate bounded phrase lookup adds
  verified current-source literal candidates to lexical retrieval. Hybrid orders fresh exact
  paths/symbols, then literal phrases, then other candidates; each class uses RRF/path ordering.
  Code reports `literal-priority+rrf(k=60)` and protected `evidence_class` metadata. Numeric RRF
  scores are not globally descending across classes. Memory lexical ranking and RRF are unchanged.
  [MCP modes, consent and the connection-local query cache](hybrid-mcp.md) describe the full contract.

## ONNX Runtime

`ort` ships no prebuilt library for macOS x86_64 and Microsoft publishes no ONNX Runtime ≥ 1.24 for
that target, so Midnight pins **ONNX Runtime 1.23.2** (FastEmbed 5.13 / `ort` rc.11 need ≥ 1.22) and
loads it dynamically. Midnight binaries do **not** embed or redistribute it.

`aether-midnight runtime install` is the only way Midnight obtains it, and it is explicit:

1. Refuses platforms without a pin (currently only `macos-x86_64` has one).
2. Downloads the official Microsoft archive named in `src/runtime.rs` over HTTPS, capped at the
   pinned size (11,676,322 bytes).
3. Compares the archive SHA-256 with the pin (`d10359e1…85a6`, equal to the digest GitHub publishes
   for the release asset) before decompressing anything.
4. Extracts only three named members (the library, `LICENSE`, `ThirdPartyNotices.txt`) and checks
   the library's own SHA-256 (`8c9c78de…c054`).
5. Writes them to `<models dir>/onnxruntime/` with a `manifest.json`. A failed install leaves
   nothing behind; re-running with a valid install is a no-op.

Library lookup order: `ORT_DYLIB_PATH`, the managed install, then `/opt/homebrew/lib`,
`/usr/local/lib`, `/usr/lib`. A managed library whose hash differs from the pin is rejected. A
library from `ORT_DYLIB_PATH` or a system path is accepted if it loads, exports `OrtGetApiBase` and
reports version ≥ 1.22, but it is *not* checksum-verified by Midnight and is reported that way.

`runtime status` and `doctor` load the candidate with `dlopen` and report, with remedies: not found,
unloadable (for example an arm64 library on an x86_64 host — the loader's message is included),
not an ONNX Runtime, too old, or tampered. Embedding commands run the same check first, so a bad
runtime is an actionable error (or a degraded notice for search), never a crash. Lexical commands
never look for the library.

Other platforms: Linux and Apple silicon builds of the same release exist upstream, but Midnight has
not been run there, so `runtime install` does not offer them and no support is claimed.

## Cloud providers

Unconfigured retrieval stays lexical; explicit embedding commands default to the local model.
Selecting `--provider openrouter` or `--provider rainy` on
`embed` or on a `--mode semantic|hybrid` search sends the text being embedded (memories, code chunks
of the named repository, or the search query) to that provider. Saved setup preferences can also
select that provider for CLI/MCP searches and indexing, with upload consent and MCP policy checks.
Lexical
modes never touch a provider, the provider is never inferred from the environment, and a missing
credential is an error instead of a fallback to another provider or model.

```sh
export OPENROUTER_API_KEY=…            # read from the environment only; never printed or stored
aether-midnight embed --namespace project:app --repo . \
    --provider openrouter --model openai/text-embedding-3-small --dimensions 1536
aether-midnight memory search --namespace project:app "disk cache" --mode hybrid \
    --provider openrouter --model openai/text-embedding-3-small --dimensions 1536

export RAINY_API_KEY=…
aether-midnight embed --namespace project:app \
    --provider rainy --model <model> --dimensions <n> [--endpoint https://host/v1]
```

| Flag | Meaning |
| --- | --- |
| `--provider local\|openrouter\|rainy` | Default `local`. |
| `--model` | Required for remote providers; never defaulted or substituted. |
| `--dimensions` | Required for remote providers. Declares the vector size, is sent as `dimensions`, and every response is checked against it. |
| `--endpoint` | API base without `/embeddings`. OpenRouter: `https://openrouter.ai/api/v1` (default). Rainy: the `rainy-sdk` default unless set. HTTPS only; plain HTTP is accepted by the SDK on loopback addresses (used by the mock tests). |
| `--timeout-secs` | Per-request timeout, default 30, maximum 300. |
| `--param KEY=JSON` | Optional request fields, allow-listed per provider: OpenRouter `input_type`, `provider` (routing object), `user`; Rainy `user`. Anything else is rejected. |

Credentials: `OPENROUTER_API_KEY` for `openrouter`, `RAINY_API_KEY` for `rainy`. `doctor` reports
only whether each is set.

**Transport.** Both providers use `rainy-sdk` 0.6.51 request/configuration types and
endpoint validation. A narrow compatibility adapter uses the existing `ureq` native-TLS
transport to retain HTTP statuses and decode optional response fields that the SDK
requires. It disables redirects, enforces a total timeout, caps successful responses at
8 MiB, and sends up to 16 texts per request. Four requests may run concurrently by default;
`MIDNIGHT_EMBED_CONCURRENCY` can tune that to 1–8. The repository code indexer gathers chunks
across files to fill those request slots while storing completed files in separate transactions.
Responses are restored to input order. Workers pull batches from a shared queue, so one slow request does not stall the others. Only rate-limit (429) and 5xx answers are retried, twice, with a 1 s then 2 s pause and always against the same provider; the first non-retryable failure stops new requests and a failed group is left
uncommitted and a later run resumes from committed files. Provider error bodies are discarded
rather than logged. Keys remain in memory only, never in output.

Contract references: [OpenRouter embeddings](https://openrouter.ai/docs/api/api-reference/embeddings/create-embeddings),
[Rainy SDK protocol](https://github.com/enosislabs/rainy-sdk), and the pinned SDK source.
Live compatibility: OpenRouter was exercised once by hand (2026-11-08) with
`qwen/qwen3-embedding-8b` at 4096 dimensions through the `setup` wizard, a consented upload, and
hybrid search on a real project of 186 indexed files (607 chunks): validation request accepted,
every file embedded (`semantic coverage: 186/186`), and relevant results came back from both the
lexical and the semantic side, including Spanish-language text. Not covered by that run: Rainy,
failure paths against the live service, incremental re-embedding, latency and cost. Rainy stays
unverified without credentials.

**Request shape.** `{"model", "input": [texts…], "encoding_format": "float", "dimensions": n}` plus the
allow-listed `--param` fields. Inputs are sent verbatim (no E5 `query:`/`passage:` prefixes — that is
the local model's contract) and truncated to 8,000 characters. Responses are placed by their
`index`, not arrival order, when every item has an index. If all indices are omitted,
array order is used; mixed indexed/unindexed responses are rejected. Optional `usage`
and metadata may be omitted. The batch must return exactly one vector per input.

**Validation.** A batch is rejected whole (nothing stored) if the response has the wrong number of
vectors, an out-of-range or duplicate `index`, base64 where floats were requested, a vector whose
length differs from `--dimensions`, a non-finite value, or an all-zero vector. Valid vectors are
L2-normalised so cosine similarity stays a dot product.

**Errors** are bounded and secret-free. HTTP status determines auth (401/403), payment
(402), rate limit (429), server (5xx), or rejected request; payment is never inferred
from message text. Timeout, network, malformed response and dimension mismatch remain
separate categories. Error bodies and provider-supplied retry instructions are not echoed.
Only 429/5xx answers are retried (twice); re-running `embed` resumes after committed vectors.

**Degraded operation.** If a provider call fails during `--mode semantic|hybrid`, the command still
exits 0 with lexical results, `degraded: true`, `mode_used: "lexical"`, a `notice` that names the
failure category, and a `warning:` line on stderr. If `embed` fails part-way it exits non-zero, keeps
the vectors committed before the failure, and says lexical search is unaffected. After
the provider recovers, searches over a partial index report `semantic_coverage` and a
`partial semantic coverage` notice (verified with 16 of 20 memories).

### Embedding-space isolation

Every stored vector carries `model`, `revision` and `dims`; queries select vectors by exact equality
on all three (plus scope) in SQL. For remote providers:

* `model` = `<provider>:<model id>` (for example `openrouter:openai/text-embedding-3-small`);
* `revision` = `remote-<12 hex>`, a fingerprint of the endpoint and the `--param` set, because either
  can change what the same model name returns;
* `dims` = `--dimensions`.

Consequences: changing provider, model, endpoint, parameters or dimensions selects a different space.
A search in a different space does **not** embed the query (nothing is sent) and degrades to lexical
with a notice that names the stored space and the requested one. Switching is an explicit action:
`embed` with the new settings deletes the scope's vectors of any other identity (`replaced_other_model`
in its report) and re-embeds. A scope therefore holds one space at a time. Memory namespaces and
repositories remain isolated from each other, as before, and `embed --namespace A` never sends
namespace B's text.

### Workflow

Embedding is never automatic. After `memory add` or `index`, run `embed` for the scope you want
searchable semantically; unchanged items are skipped by content hash. Search with the same provider
flags you embedded with.

## Privacy

Local vectors are derived from stored text and are stored in the same local database (`0600`). They are
removed with their source rows, but, like memory content, may linger in SQLite pages until `compact`.
Vectors from a cloud provider are stored the same way, but their source text was sent to that provider
under your own account and is subject to its retention terms; Midnight cannot recall it.
