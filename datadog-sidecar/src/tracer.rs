// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::exception_hash_rate_limiter::ManagedExceptionHashRateLimiter;
use crossbeam_utils::atomic::AtomicCell;
use http::uri::PathAndQuery;
use libdd_common::Endpoint;
use libdd_ipc::rate_limiter::ShmLimiterMemory;
use libdd_trace_utils::config_utils::trace_intake_url_prefixed;
use std::borrow::Cow;
use std::ffi::CString;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tracing::error;

pub(crate) struct ShmLimiters {
    pub probes: Option<Arc<Mutex<ShmLimiterMemory<()>>>>,
    pub exceptions: Option<Arc<Mutex<ManagedExceptionHashRateLimiter>>>,
}

// PHP-FPM can exit without shutting the listener down.
static EXIT_LIMITERS: AtomicCell<Option<Arc<ShmLimiters>>> = AtomicCell::new(None);

impl ShmLimiters {
    pub fn create() -> Arc<Self> {
        let probes = match ShmLimiterMemory::create(shm_limiter_path()) {
            Ok(memory) => Some(Arc::new(Mutex::new(memory))),
            Err(e) => {
                error!(
                    "Could not create the shared rate limiter: {e}. Continuing without rate limiting."
                );
                None
            }
        };
        let exceptions = match ManagedExceptionHashRateLimiter::create() {
            Ok(limiter) => Some(limiter),
            Err(e) => {
                error!(
                    "Could not create the exception hash rate limiter: {e}. Continuing without rate limiting."
                );
                None
            }
        };
        let limiters = Arc::new(Self { probes, exceptions });
        EXIT_LIMITERS.store(Some(limiters.clone()));
        unsafe { libc::atexit(unlink_shm_limiters) };
        limiters
    }
}

#[cfg(unix)]
pub(crate) fn clear_inherited_state() {
    EXIT_LIMITERS.store(None);
}

extern "C" fn unlink_shm_limiters() {
    if let Some(limiters) = EXIT_LIMITERS.swap(None) {
        if let Some(probes) = &limiters.probes {
            probes.lock().unwrap_or_else(|e| e.into_inner()).unlink();
        }
        if let Some(exceptions) = &limiters.exceptions {
            exceptions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unlink();
        }
    }
}

#[derive(Default, Clone)]
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
    CString::new(format!("/ddlimiters-{}", crate::shm_namespace())).unwrap()
}
