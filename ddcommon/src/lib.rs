// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

// TODO: fix the feature ifdef spam
#[cfg(feature = "tokio-http-connector")]
use std::borrow::Cow;

#[cfg(feature = "tokio-http-connector")]
use hyper::header::HeaderValue;

#[cfg(feature = "tokio-http-connector")]
pub mod connector;
pub mod container_id;
pub mod tag;

pub mod header {
    pub const DATADOG_CONTAINER_ID: &str = "Datadog-Container-ID";
    pub const DATADOG_API_KEY: &str = "DD-API-KEY";
}

#[cfg(feature = "tokio-http-connector")]
pub type HttpClient = hyper::Client<connector::Connector, hyper::Body>;
#[cfg(feature = "tokio-http-connector")]
pub type HttpResponse = hyper::Response<hyper::Body>;
#[cfg(feature = "tokio-http-connector")]
pub type HttpRequestBuilder = hyper::http::request::Builder;

#[cfg(feature = "tokio-http-connector")]
#[derive(Default)]
pub struct Endpoint {
    pub url: hyper::Uri,
    pub api_key: Option<Cow<'static, str>>,
}

//TODO: move into separate module
#[cfg(feature = "tokio-http-connector")]
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
