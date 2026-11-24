// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP plumbing for crash receiver uploads.
//!
//! The receiver is a dedicated, short-lived process whose environment is set
//! entirely by the tracer, which passes the proxy variables through so direct
//! (agentless) uploads honor customer egress proxies (libdatadog PR #2462,
//! refs dd-trace-js#10121). The receiver therefore reads the proxy
//! environment deliberately, once, at first use — the HTTP libraries
//! themselves never consult it (see libdd-networking's `Connector`, which is
//! always direct unless a proxy is configured explicitly).

use std::sync::LazyLock;

use libdd_capabilities_impl::{NativeCapabilities, NativeHttpClient};
use libdd_networking::connector::{Connector, Matcher};

static UPLOAD_CONNECTOR: LazyLock<Connector> =
    LazyLock::new(|| Connector::with_proxy(Matcher::from_env()));

/// The connector routing receiver uploads through the proxy environment.
pub(crate) fn connector() -> Connector {
    UPLOAD_CONNECTOR.clone()
}

/// HTTP capabilities for receiver uploads, using the proxy-aware connector.
pub(crate) fn capabilities() -> NativeCapabilities {
    NativeCapabilities::with_http(NativeHttpClient::with_connector(connector(), false))
}
