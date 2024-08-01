// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::HttpRequestBuilder;

use crate::config::Config;

pub mod header {
    #![allow(clippy::declare_interior_mutable_const)]
    use http::header::HeaderName;
    pub const REQUEST_TYPE: HeaderName = HeaderName::from_static("dd-telemetry-request-type");
    pub const API_VERSION: HeaderName = HeaderName::from_static("dd-telemetry-api-version");
    pub const LIBRARY_LANGUAGE: HeaderName = HeaderName::from_static("dd-client-library-language");
    pub const LIBRARY_VERSION: HeaderName = HeaderName::from_static("dd-client-library-version");
}

pub fn request_builder(c: &Config) -> anyhow::Result<HttpRequestBuilder> {
    match &c.endpoint {
        Some(e) => e.into_request_builder(concat!("telemetry/", env!("CARGO_PKG_VERSION"))),
        None => Err(anyhow::Error::msg(
            "no valid endpoint found, can't build the request".to_string(),
        )),
    }
}
