// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::primary_sidecar_identifier;
use datadog_ipc::rate_limiter::ShmLimiterMemory;
use datadog_trace_utils::config_utils::trace_intake_url_prefixed;
use ddcommon::Endpoint;
use http::uri::PathAndQuery;
use lazy_static::lazy_static;
use std::ffi::CString;
use std::str::FromStr;
use std::sync::Mutex;

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
    CString::new(format!("/ddlimiters-{}", primary_sidecar_identifier())).unwrap()
}

lazy_static! {
    pub static ref SHM_LIMITER: Mutex<ShmLimiterMemory> =
        Mutex::new(ShmLimiterMemory::create(shm_limiter_path()).unwrap());
}
