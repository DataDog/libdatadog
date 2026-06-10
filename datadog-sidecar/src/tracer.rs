// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::rate_limiter::ShmLimiterMemory;
use http::uri::PathAndQuery;
use libdd_common::Endpoint;
use libdd_trace_utils::config_utils::trace_intake_url_prefixed;
use std::borrow::Cow;
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};

pub static SHM_LIMITER: LazyLock<Mutex<ManuallyDrop<ShmLimiterMemory<()>>>> = LazyLock::new(|| {
    unsafe { libc::atexit(drop_shm_limiter) };
    #[allow(clippy::unwrap_used)]
    Mutex::new(ManuallyDrop::new(
        ShmLimiterMemory::create(shm_limiter_path()).unwrap(),
    ))
});

extern "C" fn drop_shm_limiter() {
    let mut guard = SHM_LIMITER.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: atexit runs once at program exit; no code accesses this static afterward.
    unsafe { ManuallyDrop::drop(&mut *guard) };
}

#[derive(Default)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
    /// Optional OTLP traces intake endpoint, used as-is (e.g.
    /// `http://host:4318/v1/traces`). When set, traces for this session are
    /// exported via libdatadog's OTLP `TraceExporter` instead of the agent
    /// msgpack `/v0.4/traces` path. Resolved by the host language (e.g. from
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) before being forwarded here.
    pub otlp_traces_endpoint: Option<Endpoint>,
    /// Headers to attach to OTLP trace export requests, parsed by the host
    /// language from `OTEL_EXPORTER_OTLP_TRACES_HEADERS`.
    pub otlp_traces_headers: Vec<(String, String)>,
    /// OTLP trace export request timeout in milliseconds
    /// (`OTEL_EXPORTER_OTLP_TRACES_TIMEOUT`). `0` means "use the default".
    pub otlp_traces_timeout_ms: u64,
}

impl Config {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        let uri = if endpoint.api_key.is_some() {
            http::Uri::from_str(&trace_intake_url_prefixed(&endpoint.url.to_string()))?
        } else {
            let mut parts = endpoint.url.into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/v0.4/traces"));
            http::Uri::from_parts(parts)?
        };
        self.endpoint = Some(Endpoint {
            url: uri,
            ..endpoint
        });
        Ok(())
    }

    pub fn set_endpoint_test_token<T: Into<Cow<'static, str>>>(&mut self, test_token: Option<T>) {
        if let Some(endpoint) = &mut self.endpoint {
            endpoint.test_token = test_token.map(|t| t.into());
        }
    }

    /// Sets the OTLP traces export configuration for this session. The endpoint
    /// URL is stored as-is (the host language resolves the full
    /// `…/v1/traces` URL). A `None` endpoint disables OTLP trace export and
    /// restores the default agent msgpack path.
    pub fn set_otlp_traces_endpoint(
        &mut self,
        endpoint: Option<Endpoint>,
        headers: Vec<(String, String)>,
        timeout_ms: u64,
    ) {
        self.otlp_traces_endpoint = endpoint;
        self.otlp_traces_headers = headers;
        self.otlp_traces_timeout_ms = timeout_ms;
    }
}

pub fn shm_limiter_path() -> CString {
    #[allow(clippy::unwrap_used)]
    CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otlp_traces_endpoint_defaults_to_none() {
        let cfg = Config::default();
        assert!(cfg.otlp_traces_endpoint.is_none());
        assert!(cfg.otlp_traces_headers.is_empty());
        assert_eq!(cfg.otlp_traces_timeout_ms, 0);
    }

    #[test]
    fn set_otlp_traces_endpoint_stores_url_as_is() {
        let mut cfg = Config::default();
        let endpoint = Endpoint::from_slice("http://collector:4318/v1/traces");
        cfg.set_otlp_traces_endpoint(
            Some(endpoint),
            vec![("api-key".to_string(), "secret".to_string())],
            5000,
        );

        let stored = cfg.otlp_traces_endpoint.expect("endpoint should be set");
        // The traces endpoint is used verbatim (unlike `set_endpoint`, which
        // rewrites the path to /v0.4/traces for the agent).
        assert_eq!(stored.url.to_string(), "http://collector:4318/v1/traces");
        assert_eq!(
            cfg.otlp_traces_headers,
            vec![("api-key".to_string(), "secret".to_string())]
        );
        assert_eq!(cfg.otlp_traces_timeout_ms, 5000);
    }

    #[test]
    fn set_otlp_traces_endpoint_none_disables_export() {
        let mut cfg = Config::default();
        cfg.set_otlp_traces_endpoint(
            Some(Endpoint::from_slice("http://collector:4318/v1/traces")),
            vec![],
            1000,
        );
        assert!(cfg.otlp_traces_endpoint.is_some());

        cfg.set_otlp_traces_endpoint(None, vec![], 0);
        assert!(cfg.otlp_traces_endpoint.is_none());
    }
}
