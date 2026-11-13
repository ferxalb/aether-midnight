# v0.2 Part 2 validation

Baseline is `dff35cb`, the Part 1 branch tip. Phase A adds schema 10 memory provenance,
policy-checked MCP recall layers, duplicate suggestions and budget packing. Phase B reuses
Tree-sitter boundaries for new code embeddings and adds cloud query timeout and retry
controls. Phase C adds experimental native distribution and npm package assembly.

## Release gates

| Gate | Status |
| --- | --- |
| Locked release build on Intel macOS | PASS |
| Strict Clippy and release tests | OPEN |
| Paired frozen retrieval, benchmark and semantic measurements | OPEN |
| Additional ONNX Runtime archive and library digests | PASS |
| Linux and Windows native builds and smoke tests | NOT RUN |
| npm publish, signing and notarization | NOT RUN |

## Open risks

Memory needs full cross-layer and held-out multilingual evidence. Duplicate suggestions
retain both observations. CLI layering, automatic review dates, a durable embedding queue,
and a separate trust-discounting score remain open. Windows repository-relative reads have
a handle-based implementation. Private Windows ACLs, named-pipe daemon support and
signing workflow integration remain open. Model selection and int8 require held-out
quality, measured memory use and runtime parity.

Before cutting the next release candidate, close the macOS retrieval and migration gates,
then run the local model bake-off. Follow with native Linux and Windows verification, and
finish with signed artifact and npm provenance checks.
