// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::rate_limiter::ShmLimiterMemory;
use datadog_trace_utils::config_utils::trace_intake_url_prefixed;
use ddcommon::Endpoint;
use http::uri::PathAndQuery;
use std::borrow::Cow;
use std::ffi::CString;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};

pub static SHM_LIMITER: LazyLock<Mutex<ShmLimiterMemory<()>>> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Mutex::new(ShmLimiterMemory::create(shm_limiter_path()).unwrap())
});

#[derive(Default)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
}

impl Config {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        let uri = if endpoint.api_key.is_some() {
            hyper::Uri::from_str(&trace_intake_url_prefixed(&endpoint.url.to_string()))?
        } else {
            let mut parts = endpoint.url.into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/v0.4/traces"));
            hyper::Uri::from_parts(parts)?
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
}

pub fn shm_limiter_path() -> CString {
    #[allow(clippy::unwrap_used)]
    CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
}
