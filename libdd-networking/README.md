# libdd-networking

Shared networking utilities for Datadog libraries.

## Overview

`libdd-networking` provides the HTTP transport layer shared across Datadog Rust libraries: endpoint
definitions, URI parsing for Datadog-specific transports, HTTP/HTTPS connectors, and a hyper-based
HTTP client.

## Features

- **HTTP/HTTPS Client**: Async HTTP client built on hyper with rustls support
- **Endpoints**: Endpoint description (URL, API key, timeout, test token) with helpers to build requests
- **URI Parsing**: Special handling for Unix sockets, Windows named pipes, and file URIs
- **Connectors**: HTTP/HTTPS, Unix domain socket, and Windows named pipe connectors

## Modules

- `connector`: HTTP/HTTPS connector implementations
- `endpoint`: Endpoint definition and URI parsing
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
