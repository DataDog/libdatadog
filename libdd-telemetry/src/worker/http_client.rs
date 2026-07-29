// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::HttpRequestBuilder;

use crate::config::Config;
use tracing::{debug, error};

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::header::HeaderName;
    pub const REQUEST_TYPE: HeaderName = HeaderName::from_static("dd-telemetry-request-type");
    pub const API_VERSION: HeaderName = HeaderName::from_static("dd-telemetry-api-version");
    pub const LIBRARY_LANGUAGE: HeaderName = HeaderName::from_static("dd-client-library-language");
    pub const LIBRARY_VERSION: HeaderName = HeaderName::from_static("dd-client-library-version");

    pub const DEBUG_ENABLED: HeaderName = HeaderName::from_static("dd-telemetry-debug-enabled");

    pub const DD_SESSION_ID: HeaderName = HeaderName::from_static("dd-session-id");
    pub const DD_ROOT_SESSION_ID: HeaderName = HeaderName::from_static("dd-root-session-id");
    pub const DD_PARENT_SESSION_ID: HeaderName = HeaderName::from_static("dd-parent-session-id");
}

/// `session_id`, then `parent_session_id`, then `root_session_id` (must match call sites in
/// `build_request`).
pub(crate) fn add_instrumentation_session_headers(
    mut builder: HttpRequestBuilder,
    session_id: Option<&str>,
    parent_session_id: Option<&str>,
    root_session_id: Option<&str>,
) -> HttpRequestBuilder {
    let Some(s) = session_id.filter(|id| !id.is_empty()) else {
        return builder;
    };
    builder = builder.header(header::DD_SESSION_ID, s);
    if let Some(r) = root_session_id
        .filter(|r| !r.is_empty())
        .filter(|r| *r != s)
    {
        builder = builder.header(header::DD_ROOT_SESSION_ID, r);
    }
    if let Some(p) = parent_session_id
        .filter(|p| !p.is_empty())
        .filter(|p| *p != s)
    {
        builder = builder.header(header::DD_PARENT_SESSION_ID, p);
    }
    builder
}

pub fn request_builder(c: &Config) -> anyhow::Result<HttpRequestBuilder> {
    match &c.endpoint {
        Some(e) => {
            debug!(
                endpoint.url = %e.url,
                endpoint.timeout_ms = e.timeout_ms,
                telemetry.version = env!("CARGO_PKG_VERSION"),
                "Building telemetry request"
            );
            let mut builder =
                e.to_request_builder(concat!("telemetry/", env!("CARGO_PKG_VERSION")));
            // Telemetry sends are heartbeat-paced (tens of seconds apart), longer
            // than the agent's HTTP keep-alive, so pooled connections are typically
            // half-closed by the next send and EOF on reuse. `Connection: close`
            // forces a fresh socket per request.
            builder = Ok(builder?.header(http::header::CONNECTION, "close"));
            if c.debug_enabled {
                debug!(
                    telemetry.debug_enabled = true,
                    "Telemetry debug mode enabled"
                );
                builder = Ok(builder?.header(header::DEBUG_ENABLED, "true"))
            }
            builder
        }
        None => {
            error!("No valid telemetry endpoint found, cannot build request");
            Err(anyhow::Error::msg(
                "no valid endpoint found, can't build the request".to_string(),
            ))
        }
    }
}
