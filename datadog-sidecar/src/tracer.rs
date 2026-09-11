// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use http::uri::PathAndQuery;
use libdd_common::{Endpoint, MutexExt};
use libdd_ipc::rate_limiter::ShmLimiterMemory;
use libdd_trace_utils::config_utils::trace_intake_url_prefixed;
use std::borrow::Cow;
use std::ffi::CString;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};
use tracing::warn;

pub static SHM_LIMITER: LazyLock<Mutex<Option<ShmLimiterMemory<()>>>> = LazyLock::new(|| {
    unsafe { libc::atexit(drop_shm_limiter) };
    Mutex::new(create_shm_limiter())
});

fn create_shm_limiter() -> Option<ShmLimiterMemory<()>> {
    match ShmLimiterMemory::create(shm_limiter_path()) {
        Ok(mem) => Some(mem),
        Err(e) => {
            warn!("Failed to create shared-memory rate limiter segment: {e}");
            None
        }
    }
}

pub fn with_shm_limiter<R>(f: impl FnOnce(&mut ShmLimiterMemory<()>) -> R) -> Option<R> {
    let mut guard = SHM_LIMITER.lock_or_panic();
    if guard.is_none() {
        *guard = create_shm_limiter();
    }
    guard.as_mut().map(f)
}

extern "C" fn drop_shm_limiter() {
    SHM_LIMITER.lock().unwrap_or_else(|e| e.into_inner()).take();
}

#[derive(Default)]
pub struct Config {
    /// Endpoint for the V0.4 trace path: agentful sessions are normalized to `/v0.4/traces`;
    /// agentless sessions point at the intake URL, which is encoding-agnostic.
    pub endpoint: Option<Endpoint>,
    /// Endpoint for the V1 trace path: agentful sessions are normalized to `/v1.0/traces`
    /// instead; agentless sessions share the same intake URL as `endpoint`.
    pub endpoint_v1: Option<Endpoint>,
    pub language: String,
    pub language_version: String,
    pub tracer_version: String,
    pub retry_interval: u64,
}

impl Config {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        let (url, url_v1) = if endpoint.api_key.is_some() {
            let url = http::Uri::from_str(&trace_intake_url_prefixed(&endpoint.url.to_string()))?;
            (url.clone(), url)
        } else {
            let mut parts = endpoint.url.clone().into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/v0.4/traces"));
            let url = http::Uri::from_parts(parts)?;

            let mut parts_v1 = endpoint.url.clone().into_parts();
            parts_v1.path_and_query = Some(PathAndQuery::from_static("/v1.0/traces"));
            let url_v1 = http::Uri::from_parts(parts_v1)?;

            (url, url_v1)
        };
        self.endpoint_v1 = Some(Endpoint {
            url: url_v1,
            ..endpoint.clone()
        });
        self.endpoint = Some(Endpoint { url, ..endpoint });
        Ok(())
    }

    pub fn set_endpoint_test_token<T: Into<Cow<'static, str>> + Clone>(
        &mut self,
        test_token: Option<T>,
    ) {
        if let Some(endpoint) = &mut self.endpoint {
            endpoint.test_token = test_token.clone().map(Into::into);
        }
        if let Some(endpoint) = &mut self.endpoint_v1 {
            endpoint.test_token = test_token.map(Into::into);
        }
    }
}

pub fn shm_limiter_path() -> CString {
    #[allow(clippy::unwrap_used)]
    CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
}
