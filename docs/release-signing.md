# Signing and Gatekeeper release gate

**Distribution decision (2026-11-07): v0.1 ships as a CLI installed by `scripts/install.sh`,
not as a signed app or PKG.** Developer ID signing and notarization are no longer part of
the v0.1 plan; the maintainer procedure below is retained only as reference for a possible
future signed distribution. The unsigned executable remains unsigned and un-notarized and is
never described otherwise; Gatekeeper behavior is documented in
[installation.md](installation.md#gatekeeper).

The RC executable has **no signature**. `codesign --verify --strict` fails and
`spctl --assess --type execute` rejects it with `source=no usable signature`.
No signing identity was available. No signing, submission, credential use or security
bypass was performed. A clean local shell launch is not a Gatekeeper acceptance test.
A browser-downloaded/quarantined install on a separate clean Mac remains unverified.

The pinned ONNX Runtime 1.23.2 library is signed by Microsoft; strict signature
verification succeeds. This corrects the earlier assumption that the upstream library
was unsigned. Midnight does not sign it. Signature validity alone does not establish
notarization or compatibility with a hardened Midnight executable. No runtime
notarization/Gatekeeper acceptance is claimed.

Apple requires a Developer ID signature and Hardened Runtime for notarization. See
[Apple's workflow](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
and [customization guide](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
A hardened CLI signed by a different team may reject the Microsoft-signed dynamic
library under library validation. **Do not ship a signed CLI before testing actual
managed-runtime loading under that policy.** Do not re-sign the pinned runtime: it
changes its SHA-256 and Midnight will correctly reject it. Do not disable library
validation or Gatekeeper as a workaround during this release phase. If this gate
cannot be resolved with an explicitly reviewed distribution/signing plan, hold release.

Maintainer procedure, only after explicit authorization and a valid identity:

1. Work on a copy of the final candidate. Record pre-sign hashes. Inspect dependencies
   with `otool -L`, signatures with `codesign -dvvv`, and entitlements. Do not put
   identity/account identifiers or notarization submission IDs in public reports.
2. Sign the CLI using an authorized Developer ID Application identity:
   `codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" bin/aether-midnight`.
   No broad entitlements are prescribed here. Test local inference against the exact
   managed runtime before proceeding; a library-validation failure blocks this route.
3. Run `codesign --verify --strict --verbose=2 bin/aether-midnight`. Re-run all packaged
   smoke tests, including model/runtime tamper rejection. Regenerate internal checksums.
4. Make an upload ZIP (`ditto -c -k --keepParent PACKAGE candidate.zip`). With separately
   authorized credentials already stored securely, submit using
   `xcrun notarytool submit candidate.zip --keychain-profile "$NOTARY_PROFILE" --wait`.
   Inspect the private notarization log even on success. Do not log credentials.
5. A bare CLI/tar archive does not provide a normal stapling target. Use an approved
   signed installer PKG or DMG if offline ticket stapling is required; notarize and
   staple that container, then verify with `stapler validate` and the appropriate
   `spctl` assessment. ZIP/tar extraction and online ticket lookup need separate tests.
6. Test a quarantined download on a clean x86_64 Mac with default protections, offline
   and online as relevant, including dynamically loaded ONNX Runtime. Stop on rejection.
   Rebuild final archive checksums after signing; unsigned RC hashes no longer apply.

This document prepares instructions only. The current candidate is **NO-GO for public
binary distribution** until signature, notarization and hardened dynamic loading pass.
