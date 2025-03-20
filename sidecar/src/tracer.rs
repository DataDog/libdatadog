// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::rate_limiter::ShmLimiterMemory;
use datadog_trace_utils::config_utils::trace_intake_url_prefixed;
use ddcommon::Endpoint;
use http::uri::PathAndQuery;
use std::ffi::CString;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

static SHM_LIMITER: OnceLock<Mutex<ShmLimiterMemory<()>>> = OnceLock::new();

pub fn get_shm_limiter() -> &'static Mutex<ShmLimiterMemory<()>> {
    #[allow(clippy::unwrap_used)]
    SHM_LIMITER.get_or_init(|| Mutex::new(ShmLimiterMemory::create(shm_limiter_path()).unwrap()))
}

#[derive(Default)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
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
}

pub fn shm_limiter_path() -> CString {
    #[allow(clippy::unwrap_used)]
    CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
}
