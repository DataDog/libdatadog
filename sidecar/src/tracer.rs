// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::config_utils::trace_intake_url_prefixed;
use ddcommon::Endpoint;
use http::uri::PathAndQuery;
use std::str::FromStr;

#[derive(Default)]
pub struct Config {
    pub raw_endpoint: Option<Endpoint>,
    pub endpoint: Option<Endpoint>,
    pub language: String,
    pub tracer_version: String,
}

impl Config {
    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        let uri = if endpoint.api_key.is_some() {
            hyper::Uri::from_str(&trace_intake_url_prefixed(&endpoint.url.to_string()))?
        } else {
            let mut parts = endpoint.url.into_parts();
            parts.path_and_query = Some(PathAndQuery::from_static("/v0.7/traces"));
            hyper::Uri::from_parts(parts)?
        };
        self.endpoint = Some(Endpoint {
            url: uri,
            api_key: endpoint.api_key,
        });
        Ok(())
    }
}
