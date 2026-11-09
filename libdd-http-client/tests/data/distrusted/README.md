# Captured certificate chains

These public certificates were captured on October 20, 2026, from the endpoints listed at
<https://emudhra.we-have-a-problem.com/>. Each affected chain contains an intermediate whose
complete DER SPKI SHA-256 and certificate SHA-256 match
<https://emudhra.we-have-a-problem.com/affected-certificates.json>.

Each PEM contains the leaf followed by the served intermediates. The six `emSign` paths
and six AAA cross-sign paths cover the DV, OV, and EV G3B/G3C issuers.
`unaffected.pem` comes from `https://testovg1.emsign.com`.

`roots.pem` deliberately trusts `emSign ECC Root CA - G3`, `AAA Certificate Services`, and
`emSign Root CA - G1`. Keeping these roots trusted ensures that rejection tests the compromised
key blocklist, rather than missing roots. The last root supports the unaffected control.
No private keys are included.

Tests use a fixed validation time of October 20, 2026, 14:40 UTC and require
`CertificateError::ApplicationVerificationFailure` for presented blocked keys. Every affected chain
must also pass the unmodified WebPKI verifier with these same roots, proving that rejection
comes from the compromised-key blocklist and not from missing roots.
Additional tests exercise blocked leaves, unused blocked intermediates, malformed certificates,
and preserved hostname, expiry, and signature validation. The data is ported from
DataDog/saluki#2857.

The integration tests (`../tls_distrust.rs`) reuse `dv-g3b-emudhra.pem` without its roots: a
local TLS server presents a freshly self-signed certificate plus the chain's first
intermediate (`EM DV TLS CA - G3B`), whose public key is compromised. The handshake would
otherwise succeed against the trust store injected through `SSL_CERT_FILE`, so the rejection
can only come from the compromised-key blocklist. This mirrors the local-server integration
test from DataDog/saluki (`lib/saluki-io/tests/certificate_distrust.rs`).
