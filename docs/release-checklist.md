# v0.1 RC checklist

Target: November 8, 2026, 08:00 America/Los_Angeles (16:00 UTC).
All artifacts stay local; all changes stay unstaged and uncommitted.

| Gate | Result |
| --- | --- |
| Provider optional fields and strict vector validation | PASS |
| HTTP 402 classified by status, no text heuristics | PASS |
| Failed batch preserves committed vectors and reports partial coverage | PASS |
| Namespace/provider/model isolation and privacy defaults | PASS |
| Formatting, strict Clippy, workspace tests, locked release build | PASS |
| Real local inference, hybrid search, model corruption rejection | PASS |
| Memory CRUD, indexing, MCP handshake, migration, cancellation | PASS |
| Frozen lexical quality suites | PASS |
| Fresh-process, warm batch, exact 10k/100k KNN, CPU/RSS | PASS |
| Reproducible archive, checksums, licenses, examples | PASS |
| Extraction and isolated local install outside repository | PASS |
| Live Rainy and OpenRouter | SKIP — no configured environment credentials |
| CLI signature / Gatekeeper | FAIL — unsigned; Gatekeeper rejects |
| Microsoft runtime signature | PASS — upstream signature verifies |
| Notarization and hardened CLI + runtime loading | SKIP — no identity; not authorized |
| Quarantined download on a separate clean Mac | SKIP — unavailable |
| Other architectures / older macOS | SKIP — support not claimed |

Decision: **NO-GO for public binary distribution** pending signing/notarization and
protected-download verification. Local unsigned RC is for maintainer review.
Detailed commands, evidence and measurements: [v0.1-status.md](v0.1-status.md).
Installation: [installation.md](installation.md). Signing: [release-signing.md](release-signing.md).
