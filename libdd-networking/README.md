# libdd-networking

Shared networking utilities for Datadog libraries.

## Overview

`libdd-networking` provides the HTTP transport layer shared across Datadog Rust libraries: URI
helpers for Datadog-specific transports, HTTP/HTTPS connectors, and a hyper-based HTTP client. The
`Endpoint` type itself lives in `libdd-common`, so crates that only describe endpoints do not
depend on this crate.

## Features

- **HTTP/HTTPS Client**: Async HTTP client built on hyper with rustls support
- **Endpoint extensions**: `EndpointExt` holds the request handling for a `libdd_common::Endpoint`: standard headers, request builders, and a reqwest client builder (requires `reqwest`). Entity headers are not part of the standard set; callers that want them opt in via `libdd-host-identity`'s `EntityHeadersExt`.
- **URI Parsing**: Special handling for Unix sockets, Windows named pipes, and file URIs
- **Connectors**: HTTP/HTTPS, Unix domain socket, and Windows named pipe connectors

## Modules

- `connector`: HTTP/HTTPS connector implementations
- `endpoint`: `EndpointExt` (`set_standard_headers`, `to_request_builder`, and `to_reqwest_client_builder` with `reqwest`); entity headers are opt-in via `libdd-host-identity`'s `EntityHeadersExt`
- `http_common`: Hyper version migration helpers
- `dump_server`: Local server that dumps received requests (requires `reqwest`)

## Feature Flags

- `https` (default): Enable HTTPS support with rustls and ring as the crypto provider; implies `http-client`
- `fips`: Use FIPS-compliant cryptographic provider (Unix only)
- `http-client`: Enable the hyper-based HTTP client
- `hyper-proxy`: Honor `HTTP(S)_PROXY`/`NO_PROXY` environment variables for the hyper connector
- `reqwest`: Enable reqwest client builder helpers and the dump server
- `tls-core`: TLS plumbing without a crypto provider; use `https` or `fips` to select one
- `tls-distrust`: Expose the compromised-key distrust verifier without the connector TLS stack
- `use_webpki_roots`: Use webpki roots instead of native certs
