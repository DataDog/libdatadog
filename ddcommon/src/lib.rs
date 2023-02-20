// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;

use hyper::header::HeaderValue;

pub mod azure_app_services;
pub mod connector;
pub mod container_id;
#[macro_use]
pub mod cstr;
pub mod tag;

pub mod header {
    pub const DATADOG_CONTAINER_ID: &str = "Datadog-Container-ID";
    pub const DATADOG_API_KEY: &str = "DD-API-KEY";
}

pub type HttpClient = hyper::Client<connector::Connector, hyper::Body>;
pub type HttpResponse = hyper::Response<hyper::Body>;
pub type HttpRequestBuilder = hyper::http::request::Builder;

#[derive(Default)]
pub struct Endpoint {
    pub url: hyper::Uri,
    pub api_key: Option<Cow<'static, str>>,
}

impl Endpoint {
    pub fn into_request_builder(&self, user_agent: &str) -> anyhow::Result<HttpRequestBuilder> {
        let mut builder = hyper::Request::builder()
            .uri(self.url.clone())
            .header(hyper::header::USER_AGENT, user_agent);

        if let Some(api_key) = &self.api_key {
            builder = builder.header(header::DATADOG_API_KEY, HeaderValue::from_str(api_key)?);
        }

        if let Some(container_id) = container_id::get_container_id() {
            builder = builder.header(header::DATADOG_CONTAINER_ID, container_id);
        }

        Ok(builder)
    }
}
