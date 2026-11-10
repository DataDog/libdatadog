# Distrusted public keys

The eight keys come from the WebPKI security fix in
[upstream commit f3caaa9f2ba94a42dcd31767e367688a416a3d87](https://github.com/rustls/webpki/commit/f3caaa9f2ba94a42dcd31767e367688a416a3d87),
which addresses RUSTSEC-2026-0382. They identify public keys, rather than
individual certificates, so issuing another certificate or cross-signing a
certificate does not bypass the check.

Upstream stores 118-byte SPKI SEQUENCE contents. Each file here adds the two bytes
`30 76` at the start, producing a complete 120-byte DER SPKI compatible with
`rustls::server::ParsedCertificate::subject_public_key_info`. No key bytes change.
The comparison happens on these fixed, length-checked fixtures; untrusted DER is
only parsed by rustls itself. See `certificate_distrust.rs` for the wrapper and the
conditions for removing this blocklist.
