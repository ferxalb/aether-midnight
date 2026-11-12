# MCP retrieval modes and literal evidence

This is the v0.1.0-rc.6 retrieval contract. It changes code ranking and adds optional network
use to MCP. It does not change the database schema or the memory lexical ranking contract.

## Defaults and activation

| Entry point | Without a saved provider | With a saved provider |
| --- | --- | --- |
| CLI `search` | lexical | hybrid |
| MCP stdio `search_code` | lexical | hybrid |
| MCP HTTP `search_code` | lexical | hybrid only if the token permits remote embeddings, or the provider is local |
| MCP `recall` | lexical | lexical; semantic/hybrid must be explicit |
| MCP `remember` / `remember_batch` | store only | store, then embed the new memories (`embed:false` skips) |

All code searches accept `lexical`, `semantic`, and `hybrid`. An explicit lexical request never
loads a model, reads credentials, or contacts a provider, even with saved cloud settings.
Invalid modes are errors, not silently ignored. A credential in the environment alone never
selects a provider. Trusted project `embeddings: false` disables both implicit and explicit
semantic code searches (explicit semantic/hybrid returns a degraded lexical response).

OpenRouter activation uses the existing provider setup, with a model and dimensions the user
chooses. For example, with `OPENROUTER_API_KEY` already set:

```sh
midnight setup --provider openrouter --model openai/text-embedding-3-small --dimensions 1536 --yes
midnight index .
midnight mcp
```

Setup's `--yes` authorizes uploads to that provider, destination and model. Setup itself performs
its existing provider validation. There is no automatic provider/model substitution. Existing
local and Rainy configurations remain valid. `midnight setup --provider lexical` restores
keyword-only defaults. `midnight mcp --local-only` disables remote calls for that process while
still permitting installed local embeddings.

MCP `search_code` examples (arguments of `tools/call`):

```json
{"name":"search_code","arguments":{"query":"Privacy-first personal intelligence","mode":"hybrid"}}
{"name":"search_code","arguments":{"query":"src/auth.rs","mode":"lexical"}}
{"name":"search_code","arguments":{"query":"where are sessions validated","mode":"semantic"}}
```

`repo_path` resolution, repository-root policy, freshness checks, limits and snippet budgets
apply in every mode. MCP accepts no provider, model, endpoint, credential or consent override.
`search_code` embeds only the query; it never embeds source files on demand. `remember` and
`remember_batch` embed only the memories they just stored, under the rules `index_repository`
uses (policy remote grant and saved consent for remote providers), with the interactive
one-attempt budget; the write is never undone by an embedding failure. With the local provider,
one MCP connection loads the model once and reuses it for every query and stored memory; earlier
builds loaded it again for each query-cache miss. With saved consent
and permission, `index_repository` also incrementally embeds the indexed repository. It keeps
its lexical report and adds `embedding` on success or `embedding_notice` on failure. A provider
failure does not undo successful lexical indexing. `refresh_indexed_file` remains lexical-only;
run `index_repository` or CLI `index` to reconcile and update embeddings afterward.

## Ranking contract

Code lexical candidate generation unions the existing path/symbol/OR-term candidates with a
separate FTS phrase lookup for queries with at least two alphanumeric terms. Each lookup is
bounded by the requested candidate limit (up to 20). Phrase candidates are verified against
the current indexed content hash and a case-insensitive contiguous literal match in source,
with alphanumeric/underscore boundaries. A phrase present only in a path, decomposed identifier
metadata, or stale source is not protected literal evidence. Punctuation and interior whitespace
are literal; this is not a whitespace-normalizing or fuzzy phrase matcher.

Code lexical ordering is: exact full path or extracted symbol, then verified literal phrase,
then remaining candidates. Within each class the existing score, raw BM25 and path ordering
apply. `match_reason` adds `literal_phrase` and `score_breakdown.literal_phrase` is `1` for that
class. This is a deliberate code retrieval contract change, including for lexical MCP clients.
No benchmark labels are changed.

Hybrid combines up to 20 lexical candidates and the best semantic chunk per file from the top
20 semantic files. Its ordering is:

1. Fresh exact full-path or extracted-symbol evidence.
2. Fresh verified literal-phrase evidence.
3. Other results, using RRF with `k=60`.

Within every class use descending RRF score, then ascending path. `fusion` reports
`literal-priority+rrf(k=60)`; protected results carry `score_breakdown.evidence_class` (`2` for
exact path/symbol, `1` for literal phrase) and an explanation in `match_reason`. The numeric
`score` remains the raw RRF value, so it is **not globally descending across evidence classes**.
Consumers must preserve the returned order. Pure semantic search and memory RRF are unchanged.

This prevents vague candidates shared by both lists from displacing a literal phrase solely
because of their double RRF contribution. It does not promise to recover every relevant file:
candidate windows, token budgets, indexing exclusions, stale files, partial vector coverage
and model relevance still bound retrieval. An exact phrase in a long file now has an independent
candidate path; it no longer needs to survive the OR-term BM25 window first.

## Consent, HTTP and failures

Every remote MCP call requires saved provider-bound upload consent. HTTP additionally requires
a token minted with `--allow-remote-embeddings`, for example:

```sh
midnight token create --name code-agent --tools search_code,index_repository \
  --repo-root . --allow-remote-embeddings
```

Existing tokens deserialize with remote permission **false**. A remote grant cannot expand the
token's tools, repository roots or memory scopes. Changing the provider, destination or model
invalidates consent. The MCP never prompts or accepts authorization from a tool argument.
The `openWorldHint` annotation is true for `index_repository`, `search_code`, and `recall` because
these tools can contact a provider; `readOnlyHint` still describes database mutation separately.

Missing vectors, a different embedding space, missing consent/credentials and provider failures
produce lexical fallback with `mode_requested`, `mode_used`, `degraded` and `notice`. Queries
are not sent when no vectors of the selected space exist. Oversized code queries are rejected
before provider loading. There is no fallback to a different provider or model.

Remote MCP query embedding uses one attempt with a five-second transport timeout, then falls
back to lexical; it does not perform bulk embedding's exponential retries. This is a transport
timeout, not a guarantee on total tool wall time (local retrieval, scheduling and context packing
also take time). Indexing retains the bulk provider timeout/retry policy and may take longer.

`recall` accepts explicit semantic/hybrid only for current-time, exact-policy recall. Historical
`at` and `broad_terms: recent` remain lexical; incompatible explicit mode combinations fail
before a provider request. Namespace authorization runs before retrieval in all modes. Memory
embedding preparation remains a CLI operation (`embed --namespace ...`); MCP never automatically
uploads newly remembered content.

## Connection-local cache

Each MCP connection caches at most 32 successful query vectors and reuses each for 60 seconds
from creation, evicting the oldest entry at capacity. Expiry is checked lazily: expired vectors
are removed on the next cache access, or when the connection closes; idle connections can retain
expired entries until then. Keys include repository or memory scope, complete model
identity (provider/model/endpoint fingerprint/dimensions), and SHA-256 of the exact query.
The cache stores vectors and hashes in process memory, never query plaintext or disk records.
Authorization and consent are rechecked on every request, including cache hits. Errors are not
cached. Lexical requests bypass the cache. HTTP resets connection state if the bearer principal
changes; closing the connection drops its cache.

Every search still retrieves from the current database and verifies source freshness; results
are not cached. Repeated queries in one connection can therefore reuse a vector without returning
old source evidence. Remote embeddings can vary after a miss, expiry, eviction or new connection;
this cache does not make the provider globally deterministic.

## Validation

`tests/mcp_search.rs` uses real stdio/HTTP product entry points against the shared deterministic
loopback provider. It covers defaults, all modes, query reuse, revoked consent, mismatched spaces,
incremental embedding, provider failures, invalid modes, project opt-out, HTTP grants, and memory
scope/time boundaries. `src/embed/tests.rs` includes an adversarial long-file phrase absent from
the semantic top 20 but preserved ahead of shared distractors, and rejection of oversized queries
before model loading. Real-provider and paired frozen-suite results are recorded separately in
[the validation report](hybrid-mcp-validation.md); synthetic embeddings are not evidence of
real-model relevance.
