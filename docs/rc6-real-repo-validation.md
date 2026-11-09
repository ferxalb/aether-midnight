# rc.6 published-artifact and real-repository validation

Validation date: 2026-11-09. The campaign exercised the installed `midnight 0.1.0-rc.6` against
this repository at the rc.6 tag. The checkout was clean before writing this report. Test
databases, downloaded assets, client configurations, raw traces and client versions remain in
ignored, access-restricted local storage. No production database, permanent client configuration,
tag or release was changed.

This report distinguishes correct fallback behavior from successful semantic retrieval. It
does not declare stable readiness or infer support for environments that were not exercised.

## Published artifact and installation

| Check | Observed result |
| --- | --- |
| rc.6 publication | Public prerelease, not a draft; matching tag workflow completed successfully |
| Published archive | Downloaded archive matches the published SHA-256 |
| Installed binary identity | Installed executable matches the executable extracted from that archive byte for byte |
| Isolated installation | Published installer succeeds without installing another runtime or replacing the user's executable |
| Repeated isolated installation | Succeeds; installed executable still matches the published asset |
| Wrong archive checksum | Rejected with exit 5 |
| Plain HTTP archive URL | Rejected with exit 2 |
| Update check | Reports current rc.6, latest rc.6, no update available |
| Real rc.5 → rc.6 update | Passed on a separate installation seeded from the published rc.5 artifact |

The first update attempt failed downloading the archive with HTTP 504. A second invocation
completed and the isolated executable reported rc.6. Some preparatory downloads also encountered
transient transport failures. These failures are retained in the private evidence; success on
retry is not a claim that the updater automatically retries every network failure.

The rc.5 seed archive and installer were verified against the published asset digests before
use. The archive download, checksum verification and replacement in the successful upgrade
were performed by the real updater, not a mock release server.

## Signing and architecture

The published executable contains both Universal 2 slices. Runtime checks in this campaign
exercise macOS x86_64 only, without a reported translated process. An arm64 runtime was not
available for validation.

`codesign --verify --deep --strict` accepts the artifact's ad-hoc signature. That is **not** a
Developer ID signature or notarization. Gatekeeper assessment rejects the executable. No
quarantine, signature or system security setting was bypassed or removed.

The successful install on this existing development machine does not replace a quarantined
browser-download test on a clean machine with default security settings.

## Indexing the real repository

The installed release first built the lexical index with `--no-embed`. The catalog was complete.
Subsequent opt-in indexing used the already configured OpenRouter provider and saved consent.
An initial transport failure left usable lexical retrieval and embeddings for 64 files. The
notice groups request failures and timeouts, so the precise network cause is not established.

A resumed invocation with concurrency temporarily reduced to two completed successfully:

| Check | Result |
| --- | --- |
| Indexed eligible files at the tested snapshot | 105 |
| Files with embeddings after recovery | 105 of 105 |
| Stored code vectors | 2032 |
| New vectors on a subsequent unchanged index | 0 |
| Reused vectors on that unchanged index | 2032 |
| Database integrity verification | `ok` |
| Indexed files under the private campaign directory | 0 |

The recovery reused committed vectors rather than starting over. This was not a controlled
concurrency comparison: it does not prove that lowering concurrency alone caused recovery.
The global provider configuration was not changed.

### Structural limitation found

`src/main.rs` is flagged as a parse-error file, leaving `structural_complete: false` while the
lexical catalog remains `complete: true`. An independent probe using the repository's cached
Tree-sitter grammar, without Midnight's cooperative parse budget, still reports error nodes
around `#[cfg(...)]` attributes inside a destructuring match pattern. The file is valid Rust
and is part of the successfully built release.

This establishes a grammar limitation for this source construct, rather than attributing the
flag solely to the 250 ms budget. Text retrieval remains available; structural extraction must
not be described as complete for this repository. No production parser behavior was changed by
this validation.

## Retrieval and real MCP clients

A literal phrase, `Deterministic Reciprocal Rank Fusion`, occurs in the long `src/embed.rs`:

| CLI mode | Observed target-file rank | Actual mode |
| --- | ---: | --- |
| lexical | 1 | lexical |
| hybrid | 1 | hybrid, not degraded |
| semantic | 6 | semantic, not degraded |

Lexical and hybrid expose literal-evidence metadata for the target. These are observations for
one real query, not a semantic-quality benchmark or a deterministic remote-rank guarantee.

Claude Code CLI and Codex CLI were run as real MCP clients using the published installed server,
temporary configuration and a read-only server policy. Their traces show actual tool calls,
not only model-written claims about calls. Both completed initialization/status, an explicit
lexical path lookup, semantic retrieval with full coverage, and fresh `open_range` reads. The
lexical path lookup returned `src/mcp/search.rs`; semantic retrieval also returned that file
first for the cache-isolation query in successful calls.

Claude Code additionally completed an explicit hybrid call after obtaining the query vector
through semantic search in the same session. In the final Codex three-mode probe, all three
remote-mode requests fell back to lexical; successful hybrid execution in that Codex probe
was therefore **not** established. Earlier standalone stdio probes did complete default hybrid,
explicit hybrid and semantic with full coverage and no degradation.

Codex invocation followed the [official MCP configuration guidance](https://developers.openai.com/codex/mcp)
and the installed CLI's help for invocation-local overrides. Exact client versions and raw
responses are retained privately, not published as environment fingerprints.

### Remote-mode availability remains an open concern

Across the recorded full-coverage stdio/HTTP/client probes, 17 authorized requests requested
semantic or hybrid retrieval (including repeated queries and cache reuse). Seven used the
requested mode; ten returned explicit degraded lexical responses with the generic network/
timeout notice. Initial incomplete-index probes and intentional policy denials are excluded.

This is a small, non-random campaign on a connection that also experienced GitHub download
failures. It is **not** an estimated production failure rate. The published MCP uses one attempt
and a five-second transport timeout, but its generic error text does not establish whether each
failure was that deadline, another transport failure, or an external service problem. Do not
attribute all ten failures solely to OpenRouter or solely to the timeout setting.

Fallback preserved useful lexical results and mode/degradation metadata. However, these probes
do not close the practical availability question for a default cloud hybrid experience. Before
stable, repeat on a known-good connection and evaluate a user-configurable query timeout and
more precise transport-error reporting. Neither change was silently applied to rc.6 here.

## HTTP authorization and memory lifecycle

Real installed-server probes verified:

- An HTTP token without the remote grant defaults to lexical; explicitly requesting hybrid is
  denied by policy through a degraded lexical response, without treating reachability as consent.
- An authorized remote HTTP token passes policy checks, but its remote attempts returned the
  generic network/timeout fallback. Successful live OpenRouter hybrid over HTTP remains unconfirmed
  in this campaign; the earlier deterministic integration tests do not substitute for that result.
- Explicit memories in a dedicated test namespace can be remembered, recalled and forgotten;
  forgotten records are suppressed. A foreign namespace is rejected.
- `open_range` returns fresh current-source lines with the API's line-number prefixes. Comparing
  the returned lines with the source, including those prefixes, succeeds.

Claude Desktop is installed, but the native computer-use connection failed to start. Its GUI
was not validated, and no Desktop configuration or session was changed. Rainy was not selected
or exercised: no Rainy credential was available in the inspected environment or credential store.

## Frozen quality and native 100k diagnostics

The release measurement harness drove the **installed published binary**, with provider-free
configuration for lexical fixtures. The code fixture's entire per-query objects, including
ordered top-five paths, match the previously validated rc.6 implementation:

| Frozen strategy | MRR | Recall@5 |
| --- | ---: | ---: |
| identifier decomposition | 0.978261 | 0.978261 |
| unicode61 comparison | 0.804348 | 0.804348 |

`memory_quality_v2` (10027 records, 25 queries, all splits, three repeats) also preserves its
complete per-query quality object: MRR 0.891667 and zero namespace leaks. This preserves existing
misses; it is not a claim of perfect retrieval.

The installed binary completed `bench-retrieval --size 100000 --samples 30` for all eight query
classes and `bench-memory --sizes 100000 --namespace-count 4`. The latter found all 50 expected
records at rank one and passed 150 cross-namespace checks with zero leaks. This closes a bounded
native x86_64 100k diagnostic run, not a new architecture or general real-world relevance guarantee.
No latency improvement is claimed from this campaign; raw resource and diagnostic output remains
private. Prior paired timing evidence, including slower cases, is in
[the implementation validation](hybrid-mcp-validation.md).

## Stable-readiness decision

Published-asset identity, isolated installation/update, real-repository retrieval, incremental
recovery, integrity, lexical fixture compatibility and the bounded native 100k diagnostics are
validated. The campaign does **not** close:

1. Practical cold-query cloud availability and successful live hybrid on every client/transport.
2. Complete structural extraction for the valid Rust pattern found in this repository.
3. Developer ID/notarized distribution and a clean-machine quarantined-download test.
4. Runtime validation on arm64, Claude Desktop GUI, and live Rainy.

Do not infer stable readiness solely from `midnight --version`, a successful build, or one
successful remote query. Resolve or explicitly scope the remaining gates in the
[release checklist](release-checklist.md). This campaign makes no runtime changes and creates
no new version, tag or release.
